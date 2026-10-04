# Capture admission and revocation

ScreenCast session creation and Screenshot requests require the supervised shell process or the installed GNOME portal backend. The compositor resolves the caller's unique D-Bus name to its bus-authenticated Unix PID. Portal admission additionally checks ownership of `org.freedesktop.impl.portal.desktop.gnome` and that `/proc/PID/exe` identifies the root-owned, non-group/world-writable installed `/usr/libexec/xdg-desktop-portal-gnome` or `/usr/lib/xdg-desktop-portal-gnome` executable. Claiming a well-known name alone does not grant capture. There is no capture test environment bypass. Window introspection shares the same admission and locked-session gate; its existing explicit unrestricted debug setting only permits metadata while unlocked.

The portal is the trusted consent broker; this check authenticates the broker, rather than implementing its picker. Applications must use the external desktop portal. The supervised shell's screenshot UI supplies explicit local selection and Capture actions. The shell's public recorder D-Bus methods deny external calls, so an arbitrary caller cannot borrow the shell's trusted compositor connection.

Each ScreenCast session belongs to its unique creator. Only that creator may add streams, start or stop it. Start is single-use, streams cannot be added after Start, and revocation is permanent. Registries admit at most 256 sessions and 64 streams per session. The compositor denies queued starts when locked or already revoked; lock closes all active PipeWire streams in its next tick, and existing offscreen capture paths also refuse locked frames. Idle dimming or blanking before lock does not itself revoke a grant.

The D-Bus service scans grants every 100 ms. Losing the creator connection, the trusted portal name, the supervised shell PID or unlocked state revokes the grant and queues stream teardown. The normal disconnect teardown latency is the scan interval plus one compositor tick; this is not a hard real-time guarantee under CPU starvation. Explicit Stop revokes immediately before queuing teardown. Closed session/stream objects are removed from the bus and registry.

The GTK proof records real UI screenshots and an explicit UI recording with decoded video frames, captures the live PipeWire node list, presses Stop, and verifies the node is removed. It also starts another UI recording before session lock and verifies both zero compositor streams and no capture node while locked. Independent untrusted clients test ordinary and forged portal-name denials. These new stages remain pending until their CI artifact passes.

The packaged-candidate `portal-security` lane verifies `xdg-desktop-portal-gnome` 51.0 in Fedora45, records runtime package versions, and runs the same genuine frontend consent lifecycle with the CI-built compositor/GTK shell, without compiling GTK locally. Its artifact ties results to the tested commit. The Ubuntu GTK lane is supplemental coverage with its distro backend version.

The proof additionally launches the root-owned installed GNOME backend and desktop portal frontend on its private bus. A real frontend client exercises CreateSession, SelectSources, Start, Cancel/Share in the actual AT-SPI picker, OpenPipeWireRemote frame consumption and owner Close; another unique caller cannot close its session. The frontend is tested again while locked. These stages are source assertions pending CI evidence.

Issue #61 remains partial until this genuine external portal journey passes and RemoteDesktop implementation/acceptance is complete. No RemoteDesktop interface is advertised by this change. The GTK proof's former raw Python caller pretending to be the portal is no longer accepted or counted as consent evidence.

On 2026-10-04 the CI-built Arch package at `95f71c0` (run
37178711148, artifact 11295256952) passed the focused genuine GNOME 51
proof locally without a Rust/GTK build. Installed versions were GNOME
portal 51.0, frontend 1.22.1, PipeWire 1.6.9, GTK 4.24.1 and libadwaita
1.10.0. It exercised actual picker Cancel/Share, FD/frame consumption,
foreign-owner denial, ordinary/spoofed caller denial, creator disconnect,
backend disconnect and active lock revocation. Observed node-removal
latencies after Close/client/backend/lock were 0.045/0.094/0.116/0.036
seconds; a new locked frontend request returned response 2. These are
individual observed values, not latency guarantees. The complete CI gate
remains pending: its first dedicated portal job stopped at ShellCheck
before runtime, and its broader GTK job failed a drag driven by fixed grid coordinates.
The held-pointer screenshot does not establish the initial allocation. The
replacement driver uses WINDOW_COORDS plus the actual grid/folder layer origin
and accepts both GTK button role names. A focused local GNOME 51 / GTK 4.24
paging probe using the same 95 package selected the visible tile at 202.5,312.5
(window bounds 146,24,113,113 plus layer origin 0,232); its real edge drag
turned the page. The broader CI journey remains required; security assertions
remain unchanged.
