---
tags: [smithay, wayland, pinning]
---

# Smithay 0.7.0 pin set for the nested slice

Pinned 2026-09-27: smithay 0.7.0, calloop 0.14.4, wayland-protocols
0.32.13, wayland-server 0.31.14, wayland-client 0.31.15,
wayland-backend 0.3.17, winit 0.30.13 (via smithay `backend_winit`,
req ^0.30.0). smithay MSRV is 1.80.1; local toolchain 1.94.1.

Verified by `cargo generate-lockfile` + `cargo check` on
`smithay { default-features = false, features = [backend_winit,
wayland_frontend, desktop] }` — compiles clean. Upstream reference
revision is git tag `v0.7.0` (anvil + tests).
