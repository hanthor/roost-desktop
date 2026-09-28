---
created_date: "2026-09-27"
document_status: draft
---

# Research — nested compositor and shell recovery

**Status:** Draft; Gate #1 pins verified 2026-09-27. Three follow-ups
remain open: EGL runtime probe on dev hardware, anvil/upstream-test
review at the pinned revision, and control-codec choice. See
“Open follow-ups”.

## 1. Smithay release, nested backend API, features, render paths — verified

Pinned 2026-09-27 (crates.io index/API; resolved and `cargo check`ed
on rustc/cargo 1.94.1):

| Crate | Pinned | Released | Notes |
|---|---|---|---|
| smithay | 0.7.0 | 2025-06-24 | MSRV 1.80.1; unyanked |
| calloop | 0.14.4 | 2026-02-13 | MSRV 1.71.1; smithay req `^0.14.0` |
| wayland-protocols | 0.32.13 | 2026-06-19 | vendors protocol XML; smithay req `^0.32.8` |
| wayland-server | 0.31.14 | 2026-07-22 | smithay req `^0.31.9` |
| wayland-client | 0.31.15 | 2026-07-22 | shell-host side |
| wayland-backend | 0.3.17 | 2026-08-14 | transitively resolved |
| winit | 0.30.13 | 2026-03-02 | via `backend_winit`, req `^0.30.0` |

- `backend_winit` = `winit` + `backend_egl` + `wayland-client/cursor/egl`
  + `renderer_gl`. Entry points `winit::init`,
  `init_from_attributes`, `init_from_attributes_with_gl_attr` return
  `(WinitGraphicsBackend<R>, WinitEventLoop)` with
  `R: From<GlesRenderer> + Bind<EGLSurface>`.
- Rendering is **GLES over EGL only**. No pixman/software path is wired
  to the winit backend in 0.7.0, so the nested preview requires working
  EGL (Mesa llvmpipe acceptable in VMs/CI).
- Upstream git tag `v0.7.0` exists for anvil/upstream-test review.
- Decision record: `docs/adr/0001-nested-backend-smithay-pin.md`.

## 2. Calloop, server setup, surface lifecycle, frames, test patterns — verified

All points confirmed against the smithay-0.7.0 crate source:

- `WinitEventLoop` implements calloop `EventSource` (winit `EventLoop`
  wrapped in `calloop::generic::Generic`); alternatively drivable
  manually via `dispatch_new_events` (`pump_app_events(ZERO)`).
- Client acceptance via `wayland::socket::ListeningSocketSource`
  (`new_auto` / `with_name`) in the same calloop loop; per-iteration
  `dispatch_all_clients` / `flush_clients` on the `Display` handle.
- Surface lifecycle traits: `CompositorHandler`, `XdgShellHandler`
  (`new_toplevel`, configure/ack), `SeatHandler`, `ShmHandler`,
  `BufferHandler`. Frame production via
  `desktop::wayland::utils::send_frames_surface_tree`.
- Test-compositor template: `examples/minimal.rs` in the 0.7.0 crate
  (nested winit + shm + xdg-shell + seat in one file). Fuller reference:
  anvil at tag `v0.7.0` (follow-up to read).

## 3. IPC envelope — proposed

Separate Unix socket (not the Wayland socket). Framed protocol: u32
length prefix + postcard/bincode body; codec choice is an open
follow-up for plan Phase 0. Proposed limits: 1 MiB max frame;
oversized/malformed/stale-version frames get a typed error and are
dropped. Backpressure via bounded server-side queue — a slow shell is
disconnected, never blocks compositor input/frame paths. Reconnect
means full snapshot plus revision resync; live-connection revision gaps
trigger resnapshot. fd-passing deferred (no current need; viable later
via rustix SCM_RIGHTS, rustix ^1.0.7 already in the smithay tree).
Decision record: `docs/adr/0002-shell-control-ipc-envelope.md`.

## 4. Activation tokens — API verified, policy proposed

API facts (`wayland::xdg_activation`, 0.7.0): token is an opaque 32-char
alphanumeric random string; `XdgActivationTokenData` carries requesting
`client_id`, seat-bound `serial: Option<(Serial, WlSeat)>`, `app_id`,
requesting `surface`, and `timestamp: Instant`.
`XdgActivationState::create_external_token` lets the compositor mint
tokens for the control API; `token_created` can veto; tokens persist
until `remove_token` / `retain_tokens` — one-use is compositor policy,
not protocol behavior. Serials differ fundamentally: per-display u32
event counters meaningful only within one client's event stream, versus
unguessable bearer strings passable across processes.

Proposed policy: 30 s expiry, one-use (remove after first successful
activation), seat binding plus `app_id` match required, log-and-deny on
mismatch. Recorded in `docs/adr/0002-shell-control-ipc-envelope.md`.

## 5. Supervision and nested lifecycle — proposed

Nested preview: compositor spawns the shell as a child process, owns a
finite restart budget with capped backoff, and kills the child on
compositor exit. A systemd user unit is deferred to production work
(004/006) and explicitly out of slice 1. Env hygiene: unique socket
name (e.g. `rwd-nested-<pid>`), `WAYLAND_DISPLAY` set for the child
only, parent host environment never mutated; nested window carries an
identifying title; host grab/ungrab escape key documented.
Decision record: `docs/adr/0003-nested-supervision-and-recovery.md`.

## 6. Recovery affordance — proposed

Slice 1 uses a compositor-owned emergency overlay on the existing input
+ GLES path (window list, shell relaunch, focus/input kept alive) —
no second client to supervise while the control contract is
provisional. A separate recovery client is deferred.
Decision record: `docs/adr/0003-nested-supervision-and-recovery.md`.

## Protocol matrix — verified

From the XML vendored in wayland-protocols 0.32.13:

| Protocol | Interface version | Needed by |
|---|---|---|
| xdg-shell stable | `xdg_wm_base` v7 | nested slice |
| viewporter | v1 | nested slice |
| presentation-time | `wp_presentation` v2 | nested slice |
| linux-dmabuf | v6 | nested slice |
| xdg-activation | v1 | token policy |
| fractional-scale | v1 | 003 follow-on |
| tearing-control | v1 | 003 follow-on |
| cursor-shape | v2 | 003 follow-on |
| single-pixel-buffer | v1 | 003 follow-on |
| xdg-dialog | v1 | 003 follow-on |
| ext-session-lock (staging) | v1 | 004 |
| linux-explicit-synchronization (unstable) | v2 | 003 |
| linux-drm-syncobj (staging) | v1 | 003 |

## Initial design constraints

Unchanged: no shell round-trip on critical motion/focus/frame paths;
shell reconnect takes a complete snapshot; revision gaps resnapshot;
control errors are typed and observable. Treat titles as untrusted
text. Separate local development assumptions from production service
identity.

## Sources pinned

- smithay 0.7.0 crate source (crates.io, released 2025-06-24;
  docs: https://smithay.github.io/smithay/smithay/index.html).
- wayland-protocols 0.32.13 vendored XML (released 2026-06-19;
  upstream: https://gitlab.freedesktop.org/wayland/wayland-protocols).
- Upstream repo tag `v0.7.0`: https://github.com/Smithay/smithay
  (anvil + tests; review pending).
- Pin verification: `cargo generate-lockfile` + `cargo check`, rustc
  1.94.1, 2026-09-27 (scratch crate, retained lockfile excerpt in
  `.spektacular/work/gate1-research/`).

## Open follow-ups

1. ~~EGL runtime probe on dev hardware~~ — done 2026-09-27, see
   “Follow-up results” below.
2. ~~Read anvil + upstream tests at tag `v0.7.0`~~ — done 2026-09-27,
   see “Follow-up results” below.
3. Control codec: postcard 1.1.3 recommended (see “Follow-up
   results”); exact frame schema and sign-off remain plan Phase 0.
4. Threat-model review stays a 004 gate; same-UID limits are labeled
   development-only in the 001 ADRs.

## Follow-up results (2026-09-27)

### EGL probe — present, headless nuance recorded

Mesa EGL 25.2.8 is installed (`libegl-mesa0`, DRI modules). `eglinfo`
reports EGL 1.5 / OpenGL_ES on the **surfaceless** platform; GBM,
Wayland, and X11 platforms fail to initialize here (no seat or host
display — expected). Consequence: the GLES nested backend can run on
this machine only under a host Wayland/X session (or Xvfb-class
stand-in for smoke tests); surfaceless EGL covers offscreen/CI paths,
not windowed WSI. Windowed WSI stays unverified until a nested run on
a live session.

### Anvil review at tag v0.7.0 — smallvil is the slice-1 template

`anvil/src/winit.rs` (`run_winit`): `EventLoop::try_new`,
`Display::new`, `winit::init::<GlesRenderer>()`, one `Output`, dmabuf
global with default feedback (v3 fallback + `bind_wl_display` for
Mesa), `AnvilState::init`, manual `dispatch_new_events` pump loop,
damage-tracked repaint. Full anvil pulls in `desktop::Space`,
`OutputDamageTracker`, dmabuf feedback, and xwayland — all deferrable
for slice 1. `smallvil/` (~480 lines: `main.rs`, `winit.rs`,
`state.rs`, `input.rs`, handlers) uses the same backend plus
`ListeningSocketSource::new_auto` and is the recommended starting
shape alongside `examples/minimal.rs`. No deviations that threaten the
plan; dmabuf/space/damage are explicit later additions, not hidden
requirements.

### Codec recommendation — postcard 1.1.3

postcard 1.1.3 (serde-native, tiny, deterministic, no config surface)
over bincode 3.0.0 (MSRV 1.85.0, config variants are a footgun for a
version-negotiated protocol). Wire plan: our own u32 length prefix,
decode from the already-capped slice so the 1 MiB limit is enforced
before deserialization. Sign-off stays with plan Phase 0.
