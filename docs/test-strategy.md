# Verification and test strategy

**Status:** Draft framework; no implementation tests have run.  
**Applies to:** all Roost speks and release gates.  
**Principle:** Every requirement has a traceable test or review artifact. Passing a protocol probe alone does not establish user-visible correctness, security, or hardware support.

## 1. Verification layers

| Layer | What it proves | Typical execution | Required evidence |
|---|---|---|---|
| Unit and property | State invariants, geometry, focus/lock transitions, revisions, quotas, version negotiation, input token validity | Every change in CI | Test ID, seed/property range, result and logs |
| Protocol/client probes | Advertised globals and behavior; malformed or adversarial client handling | CI nested compositor | Protocol/version matrix, probe output, minimized failing case |
| Nested integration | Process boundaries, app mapping, focus, reconnect, shell/extension/service crash behavior | Every relevant change in CI; repeat fault runs | Reproduction script, logs, trace IDs, before/after state |
| VM/session integration | Packaging/session startup, login lifecycle, portals, lock/auth, suspend/resume simulation | Nightly/release candidate | VM image/config, test video/logs, redacted artifacts |
| Hardware qualification | DRM/KMS, real output/input devices, drivers, mixed scale/refresh, GPU recovery | Release candidates and scheduled hardware runs | Hardware/software manifest, raw traces, known limitations |
| Human UX and accessibility | Parity, keyboard, screen reader, touch, localization, user-comprehensible errors | Design review and release candidate | Journey checklist, reviewer, recording/screenshots, discrepancy ledger |
| Security assessment | Trust boundary, spoofing, lock/capture policy, secrets, sandbox and consent | Per security-sensitive change; independent review before 1.0 | Threat/test mapping, findings and disposition, redacted logs |
| Performance and soak | Whole-session latency, memory, frame pacing, idle cost, growth | Baseline and release gates | Script revision, raw traces, repetitions, statistics, environment |
| Upgrade/recovery | Clean install, upgrade, rollback, broken config/package recovery | Release candidate per distro | Package/image version, recovery steps and data-preservation proof |

## 2. Test environments and matrix

The exact support matrix is selected in spec 000 and maintained by spec 003. At minimum, qualification should cover:

- A nested CI environment for deterministic lifecycle and protocol cases. The pinned nested backend renders GLES-over-EGL with no software fallback, so this environment must provide EGL; a software Mesa driver such as llvmpipe satisfies it without a GPU.
- At least one integrated GPU and one discrete GPU reference configuration before broad hardware claims.
- Single output and mixed 60/120 Hz multi-output where hardware supports it.
- Native Wayland GTK and Qt clients, XWayland clients, and mixed-DPI layouts at 100%, 125%, 150%, and 200% where the client/output path supports them.
- Keyboard-only, multiple layout, IME, pointer, touch, and declared tablet paths.
- Supported distro/session-service variants, portal backend, PipeWire, and user-session manager.

A matrix cell is `supported` only after evidence is recorded; `untested` is not a synonym for pass. The environment manifest includes distro/image, GNOME baseline when compared, kernel, firmware, GPU/driver, Smithay/protocol revisions, monitor model/mode/scale, input devices, locale, power profile, services, and client versions.

## 3. Required cross-cutting journeys

### Session and recovery

- Start/stop nested session and hardware session; clean logout and relaunch.
- With three application clients open, terminate the shell host. Repeat 100 times in nested automation: compositor remains responsive, all app Wayland connections remain alive, shell restarts within the configured bound, and the snapshot restores window/workspace state.
- Stall or repeatedly crash the shell; verify bounded queues, capped restart/backoff, no CPU spin, and usable recovery affordance.
- Separately crash the compositor and confirm this is reported as a session failure, not seamless app survival.

### Window, input, and output

- Focus, activation-token expiry/replay, move/resize, fullscreen/transient handling, workspace movement, Alt-Tab, gestures, output hotplug, primary-output change, and resume.
- Use native Wayland and XWayland windows through every declared scale/output configuration; assert geometry and input-coordinate alignment.
- Inject slow search, blocked service calls, stalled extension, shell disconnect, and large IPC messages while measuring compositor input/frame responsiveness.

### Security and privacy

- Spoof app IDs and privileged surface claims; connect same-UID unauthorized processes to trusted channels; attempt stale/replayed activation tokens and API major downgrade.
- While locked, kill/restart lock UI, auth helper, portal, compositor-adjacent service, switch VT, suspend/resume, hotplug, and request capture/remote input. App pixels and input remain inaccessible until valid unlock.
- Deny capture without consent; grant, start, revoke, and confirm PipeWire stops within a documented bound. Verify locked-session notification redaction.
- Search logs, crash reports, IPC traces, and extension-visible state for passwords, tokens, clipboard, pixels, and sensitive titles.
- Send oversized, malformed, high-rate, and quota-exhausting extension messages; force crash, timeout, memory/fuel exhaustion, and permission revocation.

### Accessibility and localization

Complete scripted keyboard journeys and human-reviewed AT-SPI/screen-reader journeys across panel, overview, grid/search, quick settings, notifications, lock, and recovery. Cover large text, high contrast, reduced motion, touch target sizing, RTL, localization, IME composition, on-screen keyboard, and magnification strategy. Record toolkit/widget gaps and assistive technology versions.

### Packaging and recovery

On each supported distro: clean install, session selection, login/logout, upgrade, rollback, uninstall, failed shell package, malformed config, last-known-good restore, and user-data preservation. An intentionally broken shell package/config must return to a working recovery/login path without deleting user configuration or app data.

## 4. Test design and traceability

- Assign stable IDs `Roost-<spek>-<requirement>-<case>` and link each spec requirement to unit, protocol, integration, manual, security, or performance cases.
- Every security boundary gets positive, negative, spoofing/replay, resource exhaustion, and process-failure cases.
- Every workflow gets normal, cancellation, timeout/unavailable-service, and recovery behavior where applicable.
- Property tests cover invariants rather than implementation details: no invisible/orphaned windows after output changes; no focus to unauthorized surfaces; monotonic state revisions; quota bounds; lock remains latched until valid unlock.
- Keep deterministic tests independent of wall-clock timing where possible. Timing gates use repeated samples and report distributions, not one pass/fail datapoint.
- A failure retains exact source revision, environment manifest, reproduction command, relevant redacted logs/traces, and minimized input. Never collect secrets or raw pixels unless explicitly consented test fixtures require them.

## 5. Performance and reliability gates

Spec 000 establishes distributions; spec 006 applies the project targets on the pinned reference configuration:

- p95 interactive latency and whole-session PSS no more than 10% worse than pinned GNOME baseline.
- No persistent frame-pacing regression at 60/120 Hz under the agreed workload.
- No monotonic unexplained memory growth over a 24-hour soak beyond documented bounded caches.
- Report p50/p95/p99, sample count, variance, missed deadlines, idle CPU, GPU memory, startup, and environmental state.
- Rerun noisy samples under the documented rule; do not silently discard unfavorable runs. Threshold changes require an evidence-backed ADR.

## 6. CI and release cadence

- **Per change:** format/lint/build; unit/property tests; schema/version probes; nested app mapping and critical lifecycle tests; security tests for touched boundaries.
- **Nightly/scheduled:** fuzz/protocol probes, longer shell/extension fault runs, nested soak, supported distro VM workflows.
- **Release candidate:** full hardware matrix, accessibility review, lock/portal security review, 24-hour soak, performance comparison, clean install/upgrade/rollback.
- **Release blocker:** any unresolved critical lock/capture/input/clipboard/IME/accessibility issue, app-surface loss on shell crash, unexplained growth, missing support evidence, or broken recovery path.

No implementation or test is considered complete because a test command exits successfully; its coverage and artifacts must match the requirement and supported environment being claimed.
