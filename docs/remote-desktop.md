# Remote desktop consent and input

Roost exposes GNOME's Mutter RemoteDesktop interface to the installed,
root-owned portal backend. Creating a session requires the same authenticated
portal/shell authority as capture. Every subsequent operation requires its
original unique D-Bus owner. Creation, start, EIS connection and input are denied
while locked. Sessions are single-use, with a maximum of 64 remote grants.

The GNOME portal owns the user consent picker. A linked ScreenCast session uses
`remote-desktop-session-id`, must have the same caller, and shares the remote
grant's permanent revocation flag. RemoteDesktop.Start starts its linked streams.
An unrelated caller cannot attach a stream or inject input.

Advertised devices are keyboard and pointer (bitmask 3). Legacy evdev keycodes,
relative/stream-relative absolute motion, buttons and scroll are supported.
ConnectToEIS supplies one Unix descriptor per started session. Its EIS sender
seat exposes only requested keyboard/pointer capabilities, a sealed current XKB
keymap and logical output regions. The per-grant queue is limited to 256 events;
legacy calls over that bound are rejected. EIS overflow or disconnect permanently
revokes that grant. Runtime delivery
checks the flag again, balances held key/button pairs and releases them on stop
or lock. Relative pointer motion stays inside an output.

The registry checks caller/backend loss and lock every 100 ms; the EIS worker
checks revocation on its 20 ms dispatch loop. Runtime input also checks lock and
revocation before delivery. These intervals are scheduling bounds, not real-time
guarantees. Capture nodes share the revocation flag and stop with their grant.

Touch, keysym/text injection and clipboard transfer are not implemented or
advertised. EIS output regions and keymap are snapshots at connection time;
clients need a new consent session after output/keymap changes. EIS receiver contexts are
rejected. No physical input device, GPU or VT acceptance follows from nested
software-rendered proof.

`scripts/roost-remote-security --candidate DIR --out DIR` tests a packaged
compositor and GTK shell with the installed Fedora GNOME 51 portal backend,
a real Wayland GTK input client and the independently installed libei client.
It requires actual Cancel/Allow, a linked PipeWire frame, foreign input denial,
legacy and EIS keyboard/button delivery, node removal after Close/client/backend
loss/EIS disconnect/lock, and new-session lock denial. Candidate commit, runtime
versions, consent screenshots, input logs and frame evidence accompany the run.
Until this gate passes on the published candidate, these are pending acceptance
checks and issue #61 remains open.
