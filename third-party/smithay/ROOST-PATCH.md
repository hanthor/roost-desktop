# Roost Smithay 0.7.0 socket preparation patch

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

Only src/xwayland/mod.rs and xserver.rs differ from upstream. The socket
handoff regression test sends real bytes before transferring the owner.
This source lives outside Cargo's registry vendor directory so factory
cargo vendor --locked vendor remains a separate offline dependency closure.
