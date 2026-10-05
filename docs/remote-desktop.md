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
clients need a new consent session after output/keymap changes. Rebinding is
limited to 32 device generations per connection to bound unacknowledged object
and outgoing keymap-FD churn. The vendored reis 0.7.1 transport additionally fails
when undecoded incoming or unread outgoing bytes exceed 2 MiB or FD
storage exceeds 64; FD-clone failure closes the connection; its upstream
1 MiB message limit remains. See third-party/reis/ROOST-PATCH.md and regression tests. EIS receiver contexts are
rejected. No physical input device, GPU or VT acceptance follows from nested
software-rendered proof.

`scripts/roost-remote-security --candidate DIR --out DIR` tests a packaged
compositor and GTK shell with the installed Fedora GNOME 51 portal backend,
a real Wayland GTK input client and the independently installed libei client.
It requires actual Cancel/Allow, a linked PipeWire frame, foreign input denial,
legacy and EIS keyboard/button delivery, node removal after Close/client/backend
loss/EIS disconnect/lock, and new-session lock denial. Candidate commit, runtime
versions, consent screenshots, input logs and frame evidence accompany the run.
The exact `1f441bb` Remote head passed all sixteen checks in [CI 37214121497](https://github.com/hanthor/roost-desktop/actions/runs/37214121497). The combined Screenshot/Remote candidate `3249f362` passed the same complete GNOME 51 journey in [CI 37214394538](https://github.com/hanthor/roost-desktop/actions/runs/37214394538), including independent installed libei 1.6 keyboard and pointer delivery. See `remote-desktop-evidence.json` for candidate, runtime versions and retained artifacts. The proof requires fresh key/button events after each delivery baseline, then no new client input and an empty held seat after backend/lock revocation; a frontend void-method reply alone is not an admission oracle. New locked sessions are denied at CreateSession with response 2.

Issue #61's stated consent, stream, revoke and locked-denial acceptance is complete. The unsupported capabilities above remain broader GNOME parity limits, and P-SY-05 stays partial.
