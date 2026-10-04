# GNOME portal native-display bootstrap

The installed Ubuntu GNOME portal backend requires Mutter's `org.gnome.Mutter.ServiceChannel` interface before it creates a native GTK display. Its `OpenWaylandServiceConnection` method takes portal service type `1` and returns a Unix file descriptor (`(u) -> h`). The backend opens this connection before acquiring its well-known portal bus name. GNOME 51's separate reference journey does not exercise this older startup requirement.

The genuine GTK run for capture PR #252 at `e4bea38e2a2522e3555a24a285bbf81b180f6a14` failed `G-PORTAL-CONSENT` on 2026-10-04. The installed backend logged “Non-compatible display server, exposing settings only”; the frontend reported a missing ScreenCast interface and returned CreateSession response 2. This occurred before a picker existed. The same run passed native no-XWayland startup, server/XIM restart, long-folder viewport and held edge paging, native IBus composition/candidates, and genuine GTK3 XIM.

Roost provides the requested ordinary Wayland connection only to a D-Bus caller whose PID resolves to the installed, root-owned, non-writable GNOME portal executable. This bootstrap cannot require the portal's not-yet-acquired well-known name. Connections use ordinary client permissions, not the IME bridge or shell role. There is one live connection per unique caller and a maximum of 32; disconnected connections free their slots. Unsupported service types are rejected.

Opening a display is not capture admission. The existing ScreenCast methods still require the current unique portal owner or supervised shell, enforce session ownership, and deny or revoke capture while locked. A backend may initialize its ordinary display while locked, just as ordinary Wayland clients may connect, without gaining a capture grant.

The real security client checks denial for both an untrusted caller and a forged portal service name. The full current-head GNOME picker Cancel/Share, frame consumption, owner Close, lock and disconnect proofs remain required before acceptance.

Upstream contracts: [Mutter 46 ServiceChannel](https://raw.githubusercontent.com/GNOME/mutter/46.0/data/dbus-interfaces/org.gnome.Mutter.ServiceChannel.xml), [GNOME portal 46 native display initialization](https://raw.githubusercontent.com/GNOME/xdg-desktop-portal-gnome/46.0/src/externalwindow-wayland.c).
