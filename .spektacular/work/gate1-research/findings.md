# Gate #1 research findings — provisional pins for 001 implementation

Date: 2026-09-27. Status: verified where marked **[verified]** (source
inspected or command run); **[proposal]** where it is a design input for
plan review, not an upstream fact. Nothing here changes a store file;
promotion into `research.md`/ADRs needs the spektacular CLI.

## 1. Version pins (all [verified] via crates.io index/API + cargo resolve + cargo check)

| Crate | Pinned | Released | Role |
|---|---|---|---|
| smithay | 0.7.0 | 2025-06-24 | compositor framework |
| calloop | 0.14.4 | 2026-02-13 (MSRV 1.71.1) | event loop |
| wayland-protocols | 0.32.13 | 2026-06-19 | protocol bindings + vendored XML |
| wayland-server | 0.31.14 | 2026-07-22 | server bindings |
| wayland-client | 0.31.15 | 2026-07-22 | shell-client bindings |
| wayland-backend | 0.3.17 | 2026-08-14 | transitively resolved |
| winit | 0.30.13 | 2026-03-02 | nested backend window (via `backend_winit`, req `^0.30.0`) |

- smithay 0.7.0 MSRV is 1.80.1; local toolchain is rustc/cargo 1.94.1.
- `cargo generate-lockfile` + `cargo check` on
  `smithay@0.7.0 { default-features=false, features=[backend_winit,
  wayland_frontend, desktop] }` + the above pins: **compiles clean**
  (2m30s, scratch crate in /tmp/gate1/pin-check, since removed from repo).
- Upstream git tag `v0.7.0` exists (github.com/Smithay/smithay) — anvil
  reference + upstream tests are reviewable at that exact revision
  (follow-up, not yet read).

## 2. Nested backend ([verified] from smithay-0.7.0 source)

- `backend_winit` = `winit` + `backend_egl` + `wayland-client/cursor/egl`
  + `renderer_gl`. Entry: `winit::init` / `init_from_attributes` /
  `init_from_attributes_with_gl_attr` → `(WinitGraphicsBackend<R>,
  WinitEventLoop)` where `R: From<GlesRenderer> + Bind<EGLSurface>`.
- Rendering path is **GLES over EGL only**. No pixman/software path is
  wired to the winit backend in 0.7.0. Nested preview therefore
  **requires working EGL** (Mesa llvmpipe acceptable in VMs/CI).
- Follow-up: this machine has Vulkan ICDs but **no libEGL in
  ldconfig** — install Mesa and run an EGL runtime probe before
  promising nested builds here.

## 3. Event loop / server setup ([verified] from source)

- `WinitEventLoop` implements calloop `EventSource` (wraps winit
  `EventLoop` in `calloop::generic::Generic`; also drivable manually via
  `dispatch_new_events` with `pump_app_events(ZERO)`).
- Client acceptance: `wayland::socket::ListeningSocketSource`
  (`new_auto` / `with_name`), inserted into the same calloop loop.
- Server dispatch follows the socket.rs doc pattern: `Display` +
  `DisplayHandle`, `dispatch_all_clients` / `flush_clients` per loop
  iteration.
- Surface lifecycle traits: `CompositorHandler`, `XdgShellHandler`
  (`new_toplevel`, configure/ack), `SeatHandler`, `ShmHandler`,
  `BufferHandler`; frame production via
  `desktop::wayland::utils::send_frames_surface_tree`
  (per-surface `send_frame` equivalent).
- Test-compositor template: `examples/minimal.rs` in the 0.7.0 crate
  (nested winit + shm + xdg-shell + seat in one file). Upstream anvil at
  tag `v0.7.0` is the fuller reference (follow-up to read).

## 4. Protocol matrix for the pin ([verified] from vendored XML in wayland-protocols 0.32.13)

- Nested slice needs: `xdg_wm_base` v7, `wp_viewporter` v1,
  `wp_presentation` v2, `zwp_linux_dmabuf_v1` v6, `xdg_activation_v1` v1.
- Also bundled: fractional-scale v1, tearing-control v1,
  cursor-shape v2, single-pixel-buffer v1, xdg-dialog v1,
  `ext_session_lock` v1 (staging path, for 004),
  `zwp_linux_explicit_synchronization` v2 (unstable),
  `wp_linux_drm_syncobj` v1 (staging, for 003).

## 5. Activation tokens ([verified] API, [proposal] policy)

API facts (`wayland::xdg_activation`, 0.7.0):
- Token is an opaque 32-char alphanumeric random string (`rand`); exposed
  as `XdgActivationToken::as_str` / `Deref<str>`.
- `XdgActivationTokenData`: requesting `client_id`, `serial:
  Option<(Serial, WlSeat)>` (seat-bound input/focus serial), `app_id`,
  requesting `surface`, `timestamp: Instant`.
- `XdgActivationState::create_external_token` lets the compositor mint
  tokens itself (control-API path). `token_created` can veto; tokens
  persist until `remove_token` / `retain_tokens` — **one-use is
  compositor policy, not protocol behavior**.
- Difference from Wayland serials: serials are per-display u32 event
  counters, only meaningful within one client's event stream; tokens are
  unguessable bearer strings passable across processes (launcher → app).

Proposed policy for plan review: 30 s expiry, one-use (remove after
first successful activation), require seat binding + `app_id` match;
log-and-deny on mismatch. Control API mints via
`create_external_token` with purpose/app_id attached.

## 6. IPC envelope ([proposal] for plan review)

- Transport: dedicated Unix socket (not the Wayland socket), framed
  protocol: u32 length prefix + postcard/bincode body (exact schema
  codec TBD in plan Phase 0).
- Proposed limits: 1 MiB max frame; oversized/malformed/stale-version
  frames → typed error + drop ( matches plan Phase 2 evidence rule).
- Backpressure: bounded server-side queue; slow shell gets
  disconnected, never blocks compositor input/frame paths.
- Reconnect = full snapshot + revision resync (no incremental catch-up
  across disconnects); revision gaps on live connection → resnapshot.
- fd-passing deferred (no current need); viable later via rustix
  SCM_RIGHTS — rustix ^1.0.7 already in the smithay tree.

## 7. Supervision + nested lifecycle ([proposal] for plan review)

- Nested preview: compositor spawns the shell as a child process
  (`std::process::Child`), owns restart budget (finite attempts, capped
  backoff), and kills it on compositor exit. systemd user unit is
  deferred to production (004/006) — explicitly out of slice 1.
- Env hygiene (nested-session safety): unique socket name
  (e.g. `rwd-nested-<pid>`), set `WAYLAND_DISPLAY` for the child only,
  never mutate the parent's host `WAYLAND_DISPLAY`; nested window gets
  an identifying title; document the host grab/ungrab escape key.

## 8. Recovery affordance ([proposal] for plan review)

- Slice 1: compositor-owned emergency overlay (existing input + GLES
  path, no second client to supervise): shows window list, offers
  shell relaunch, and keeps focus/input alive. A separate recovery
  client is deferred — one less supervised process while the control
  contract is still provisional.

## Follow-ups (owner: plan Phase 0, re-check 2026-10-11)

1. Install Mesa on dev hardware; run EGL probe (`eglinfo` / tiny
   winit window) — blocks nested builds here.
2. Read anvil + upstream tests at tag `v0.7.0`; record deviations.
3. Codec choice (postcard vs bincode) + exact frame schema in plan.
4. Threat-model review stays a 004 gate; same-UID limits labeled
   development-only in the 001 ADR.
