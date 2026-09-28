---
tags: [smithay, winit, egl]
---

# The winit nested backend has no software fallback

smithay 0.7.0 `backend_winit` renders GLES-over-EGL only
(`WinitGraphicsBackend<R: From<GlesRenderer> + Bind<EGLSurface>>`).
No pixman/software path is wired to it, so the nested preview needs
working EGL — Mesa llvmpipe is acceptable in VMs/CI. EGL probe
2026-09-27: Mesa EGL 25.2.8 present, EGL 1.5 + OpenGL_ES on the
surfaceless platform; GBM/Wayland/X11 fail headless (expected, no seat
or host display). Windowed nested runs need a host Wayland/X session;
windowed WSI is unverified until a run on a live session.
