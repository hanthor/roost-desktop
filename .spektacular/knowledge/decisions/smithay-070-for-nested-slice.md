---
tags: [smithay, compositor]
---

# Use Smithay 0.7.0 for the 001 nested slice

Chosen 2026-09-27 over staying on the older 0.5.x line: 0.7.0 is the
latest unyanked release, carries `XdgActivationState::create_external_token`
(compositor-minted activation tokens for the control API),
`send_frames_surface_tree`, and a calloop-native `WinitEventLoop`
(`EventSource` + manual `dispatch_new_events`). wayland-protocols
0.32.13 vendors the XML we build against (xdg-shell v7, ext-session-lock
v1, drm-syncobj v1), so protocol facts come from the crate source, not
search snippets. Revisit only if 0.7.x is yanked or a later release
changes the winit/calloop API.
