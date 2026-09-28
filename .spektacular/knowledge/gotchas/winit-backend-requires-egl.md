---
tags: [smithay, winit, egl]
---

# The winit nested backend has no software fallback

smithay 0.7.0 `backend_winit` renders GLES-over-EGL only
(`WinitGraphicsBackend<R: From<GlesRenderer> + Bind<EGLSurface>>`).
No pixman/software path is wired to it, so the nested preview needs
working EGL — Mesa llvmpipe is acceptable in VMs/CI. This dev
machine has Vulkan ICDs but no libEGL: install Mesa and run an EGL
probe before expecting nested builds to run here.
