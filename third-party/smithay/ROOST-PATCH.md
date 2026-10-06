# Roost Smithay 0.7.0 additive patches

Source: published crates.io smithay 0.7.0, upstream tag v0.7.0,
crate VCS commit a166cf4c94b5aedc332a65aa1dd753e8148829c3.
Published crate SHA256: 740cea6927892bc182d5bf70c8f79806c8bc9f68f2fb96e55a30be171b63af98.
Original MIT license retained as LICENSE.txt. Version remains 0.7.0.

Roost issue #219 needs to advertise a reserved X11 display before starting
XWayland. The additive XWaylandSockets owner prepares and retains Smithay's
existing display lock and listening sockets without spawning a process.
Borrowed listening FDs permit readiness watches. XWayland::spawn_on_sockets
consumes that exact owner, retaining queued first-client bytes and Smithay's
private ClientData construction. The existing spawn API delegates unchanged.

The socket preparation patch changes src/xwayland/mod.rs and xserver.rs. The socket
handoff regression test sends real bytes before transferring the owner.
This source lives outside Cargo's registry vendor directory so factory
cargo vendor --locked vendor remains a separate offline dependency closure.

Issue #62 additionally changes src/backend/drm/surface/gbm.rs; its isolated
source diff is retained in SLEEP-RESET-API-PATCH.diff. The additive
GbmBufferedSurface::discard_pending_frames API removes pending_fb, queued_fb
and next_fb without calling submit or marking a buffer presented. It returns
at most two user-data entries so Roost can explicitly discard abandoned
presentation feedback. current_fb remains retained until KMS reset. This
avoids frame_submitted's implicit queued-frame submission during S3 recovery;
Roost resets the buffer pool and KMS state, then drains obsolete queued DRM
vblank completions before queuing the locked scene. Existing APIs and the
pinned 0.7.0 version remain unchanged.

Issue #346 additionally changes src/input/keyboard/mod.rs. The additive
KeyboardHandle::input_discard API advances physical/XKB state without
calling client or grab input, and removes consumed releases from the held
keys advertised on subsequent focus enter. Modifier changes from intercepted
input are retained until the next forwarded event, so a wake shield cannot
leave the focused client with a stale modifier mask. Real wl_keyboard tests
cover consumed releases, focus-enter key arrays, and same-focus modifier
updates without forwarding the consumed event. The pinned version stays 0.7.0.
