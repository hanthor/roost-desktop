# Where Tuna's CPU goes (2026-10-08)

Issue #503 asks Tuna Desktop to use less CPU than GNOME 51 on the same Marlin VM. This report attributes the measured CPU to processes and workload phases, traces the largest costs to code, and records the two fixes landed with it. It reanalyses existing raw samples and does not add a new paired run. The fixes still need a paired run on a main package (see "Verification still needed").

## Evidence

The guest observer records `cpu_ticks` (utime plus stime at 100 Hz), `ppid`, start time and PSS for every process of the test user, once per second. It does not record process names. The table below identifies processes by their tree position and start time. `roost-session` starts the compositor (`ppid` = the session launcher). The compositor spawns `roost-shell-gtk` about one second later. Applications launched from the shell are children of the shell.

The analysis divides tick deltas by guest `CLOCK_BOOTTIME` deltas, where 100% is one core. To reproduce, decompress with `gzip -dc` and group by `pid`.

### 2026-10-04 fresh pair (package `7546647`)

This is the run quoted in #503. It used 159% user CPU against GNOME's 19%.

| Process | Mean CPU over its lifetime | Share of user CPU |
|---|---:|---:|
| roost-compositor (pid 1056) | 144% | about 90% |
| first launched app (pid 1696, child of shell) | 10% | |
| roost-shell-gtk (pid 1105) | 6.4% (bursts at startup only) | |
| gnome-shell (pid 1127, GNOME run) | 16.7% | |

During the settle and idle phase (25–50 s), before any input, the compositor used 125–138% while the shell used 0%. GNOME Shell used 0% in the same phase. The compositor repainted continuously with nothing changing. The [native cadence report](2026-10-04-native-cadence/README.md) measured pure idle at 130% on the same package. That package predates the idle-repaint work: the scene signature skip in `crates/compositor/src/native_repaint.rs` and the buffer-age damage that followed it.

### 2026-10-06 pair at `417b77a` (after the idle-repaint work)

| Phase (guest seconds) | roost-compositor | roost-shell-gtk | gnome-shell |
|---|---:|---:|---:|
| Pure idle (50–79) | about 0% | 0% | 0% |
| Native frame-pacer probe (80–90) | 30% | **22%** | 16% |
| App launches (95–112) | 30% | 9% | 17% |
| Overview toggles and workspace switches (114–150) | **58%** | 2% | 19% |
| Workspace switches and settle (150–174) | 36% | 1% | 27% |
| Notifications (174–186) | 40% | 1% | 6% |
| Final settle, three apps open (190–216) | **37%** | 0% | 4% |
| Whole run mean | 27.5% | 2.8% | 10.8% |

Idle is fixed: the compositor is at parity with GNOME at idle. The remaining gap comes from the compositor under workload. The largest single phase is the overview. After the workload, with Disks, System Monitor and Files open and no input, the compositor stays at 37% while gnome-shell drops to 4%. System Monitor itself uses 5% in both sessions, so its commit rate is comparable. Per committed frame, Tuna costs about 9× what Mutter costs. The shell's 22% during the frame-pacer probe is a second, separate cost.

## Findings

### 1. Overview transition re-rendered the wallpaper card in software every frame (fixed)

During the 250 ms overview transition, `overview::transition` interpolates the active workspace card from the full output to its settled rect, so the card's size changes every frame. `Wallpaper::card_element` cached cards by drawn size. Every transition frame therefore ran `roost_wallpaper::card` on the compositor thread: a per-pixel crop, a Triangle resize, a rounded mask and an `image::imageops::blur` shadow. The cache holds four entries, so the transition frames also evicted the settled active and neighbour cards, which were then rendered again on every open.

The wallpaper crate is built at `opt-level = 3` even in dev builds. Timed on the investigation host (shared 2-core VM), one card takes:

| Card size | Time |
|---|---:|
| 1280×800 (transition start) | 390 ms |
| 1100×680 | 334 ms |
| 922×553 (settled active card) | 194 ms |
| 867×520 (settled neighbour) | 164 ms |

Each transition frame therefore blocked the event loop for hundreds of milliseconds. This matches the observed numbers:

- The overview visible-response median was 382 ms against GNOME's 159 ms. The first transition frame alone takes about one card render.
- The overview phase used 58% compositor CPU.
- Presentation intervals were long during the workload.

**Fix (`perf(compositor): render overview cards once at their settled size`):** `WorkspaceCard` now carries its `settled` size. The card is rendered once at that size and drawn scaled to the transition rect. The corner radius and shadow already scale linearly with the card, so a scaled settled card has the same geometry. Settled frames are pixel-for-pixel unchanged, because the buffer is drawn unscaled when the rect equals the settled size. Transition frames are a GPU scale of the same image. After the first open, an overview open or close does no software rendering at all.

The first overview open per wallpaper and output size still renders the active card and its neighbours once, about 0.5 s in total. See remaining item R2.

### 2. Every flip was a whole-plane KMS update (fixed)

`queue_buffer` was called with no damage, so the kernel received no `FB_DAMAGE_CLIPS` and treated every flip as a full-plane update. Shadow-buffered KMS drivers then copy the whole frame every flip. The compositor already repaints only the buffer-age damage, so it now submits that region as the plane damage. That region covers both the change from the previous frame and everything stale in this buffer, so it is correct for drivers that keep one shadow scanout and for drivers that upload each buffer separately.

The benefit on the CI VM is small. virtio-gpu forces a full upload when the framebuffer changes, so most of the saving there is on the host. Physical shadow-buffered drivers (simpledrm, ast, mgag200, udl) benefit directly.

### 3. Overdraw: no occlusion culling in `draw_scene` (open, largest steady-state item)

`draw_scene` clears the damage, draws the full-output wallpaper, then draws every window and layer from bottom to top, all clipped only to the damage rectangle. Nothing beneath an opaque window is skipped. On llvmpipe, fill rate is the frame cost.

Consider a single client updating a region over a maximised or stacked window. Each frame clears it, blends the wallpaper over it, then every window under it, then the window itself. Mutter clips each actor to its unobscured region and paints only what is visible. This is the most likely cause of the 37% steady state with three apps open, and of the "9× per frame" ratio above.

Smithay's `OutputDamageTracker::render_output` already does opaque-region culling. Roost uses `damage_output` only for the damage and then draws everything itself.

### 4. Damage is collapsed to one bounding rectangle (open)

`native_repaint::damage_region` merges all damage into one rectangle. Consider a clock tick in the top bar and a progress bar at the bottom of a window: they repaint nearly the whole output. Keeping the rectangle list (clipped and coalesced) avoids that. The scanout damage from finding 2 would then become a list as well.

### 5. Per-wakeup work in `Runtime::tick` (open)

The loop calls `tick()` after every calloop dispatch: client requests, page flips, input, and at least every 16 ms (`FRAME_BUDGET`). Each tick does all of the following, even when nothing is drawn:

- Reads the `roost-idle-blank` runtime signal file: open, fstat, read and close.
- Builds the full scene for every output not waiting on a flip: `scene_elements`, `backdrop`, previews and the frame signature.
- `backdrop` calls `Wallpaper::element`, which opens, reads and JSON-parses the wallpaper drop file. It also formats a cache key. In the overview, `card_element` repeats that once per card.
- `publish_cast_outputs` rebuilds the output and window snapshots, cloning every title, for Introspect.
- Computes `frame_roots` and `pending_output_work` surface-tree walks.

At idle this costs only about 62 wakeups/s and is invisible (0.48% idle mean). A busy client generates several wakeups per frame, though, so this cost scales with client activity. Change-driven caching (an inotify watch on the drop file, dirty flags for the snapshots) or gating the scene build on a damage or commit flag would remove most of it.

### 6. Shell-side costs (open, needs names in samples)

- `roost-shell-gtk` used 22% while the native frame-pacer probe ran: a new 320×180 window committing every frame. It used 9% during app launches. Something in the shell reacts per window event or per frame. Candidates are the window list or dock refresh and icon lookup for an app without a desktop file. Confirming this needs a profile.
- The shell polls the control socket on a 16 ms GLib timer (`crates/shell-gtk/src/main.rs`), with 250 ms timers for DND and notification ticks and a 100 ms tray poll. Measured CPU is about 0% at idle, but these timers wake the process about 80 times per second. Use an fd watch (`glib::unix_fd_add_local`) for the control socket instead.

### 7. Not a factor

- With `ROOST_COMPOSITOR_STATE` unset (the Marlin perf image boots the plain `roost.desktop` session), `publish_state` returns immediately. It costs nothing in the measured sessions.
- `ROOST_PERF_TRACE` is not set in the perf image, so its overhead is absent.
- The lock-screen blur is cached per wallpaper and size (`lock_element`). It costs once per lock, not per frame.

## Remaining fixes, ranked by expected impact

| Rank | Item | Expected impact | Needs design? |
|---|---|---|---|
| R1 | Opaque-region occlusion culling in `draw_scene` (finding 3), or rendering through `OutputDamageTracker::render_output` | Largest steady-state saving under workload. Likely 2–4× less fill per frame with stacked windows. | Yes. It touches the pass layout, decor, previews and lock. |
| R2 | Pre-render settled overview cards off-thread when the wallpaper or layout changes, and speed up `roost_wallpaper::card` (box-blur approximation, blur only the margin band) | Removes the remaining one-time 0.2–0.5 s stall on the first overview open. | Small design: off-thread like the decode, with a visual-parity check on the shadow. |
| R3 | Keep multi-rectangle damage (finding 4) | Large for scattered small updates (clock plus app). | No, but needs careful tests. |
| R4 | Change-driven `tick()` (finding 5): cache the drop file by inotify or identity, skip scene build when nothing committed, dirty-flag `publish_cast_outputs` | Scales with wakeup rate. Moderate under busy clients. | Small. |
| R5 | Profile and fix the shell's per-window/per-frame work (finding 6) | Up to 22% during window churn. | Needs a profile first. |
| R6 | Replace shell 16/100/250 ms polling timers with fd and signal sources | Wakeups and power more than CPU. | No. |
| R7 | Throttle frame callbacks for windows on inactive workspaces or fully occluded | Stops hidden animating clients driving frames. | Needs a policy decision (GNOME parity). |

## Before numbers for these fixes (2026-10-08)

[Paired run 37788482241](https://github.com/tuna-os/tuna-desktop/actions/runs/37788482241) was dispatched from this branch. As the lane requires, it measured the latest trusted main package (run 37713271797, source `c9be27dd`), not these fixes. It uses three fresh VM pairs per profile:

| Profile, pair | GNOME CPU p50 / p95 | Tuna CPU p50 / p95 | GNOME overview p50 / p95 (ms) | Tuna overview p50 / p95 (ms) |
|---|---:|---:|---:|---:|
| diagnostic, 1 | 17.0 / 63.0 | 50.9 / 98.0 | 166 / 503 | 428 / 781 |
| diagnostic, 2 | 17.9 / 65.0 | 51.0 / 96.0 | 161 / 394 | 353 / 783 |
| diagnostic, 3 | 18.0 / 60.0 | 52.0 / 95.0 | 167 / 652 | 356 / 616 |
| stock, 1 | 18.0 / 68.0 | 53.0 / 93.0 | 211 / 495 | 395 / 658 |
| stock, 2 | 18.0 / 70.0 | 53.0 / 93.0 | 167 / 489 | 339 / 500 |
| stock, 3 | 19.0 / 64.0 | 51.0 / 91.0 | 165 / 547 | 409 / 656 |

CPU is whole-run user CPU in percent of one core, including startup and workload. Overview figures are QMP-observed upper bounds. Current main uses about 3× GNOME's median CPU, down from 8× in the run quoted in #503. The overview median stays about 2.2× GNOME's, which is consistent with finding 1.

## Verification still needed

The paired lane only accepts trusted main packages. The fixes in this PR therefore need a `performance-baseline.yml` dispatch after merge. Do not draw conclusions until that run is committed beside this report. Expected effects:

- Overview visible response close to GNOME's.
- A clear drop in compositor CPU during the overview phase.
- Little change in the three-app steady state, which waits on R1.

## Proposal for the #315 regression gate

1. **Per-process attribution in the observer.** Record `comm` (and `cmdline[0]`) alongside `pid` in `roost-perf-guest.py`. Without names, every report has to reconstruct the process tree by hand.
2. **Phase-segmented CPU.** The guest already marks idle boundaries. Mark the overview, workspace-switch, notification and final-settle phases the same way, and report per-process CPU per phase. The whole-run median mixes startup with workload and hid the fact that idle was already fixed.
3. **Gate on main pushes (nightly), against the committed GNOME 51 reference from the same image.** Fail the run when any of these holds on two consecutive nights, to absorb runner variance:
   - Pure-idle compositor CPU p95 is above 2%, or above GNOME's p95 plus 1 point.
   - The overview phase's compositor-plus-shell CPU mean is above GNOME's gnome-shell mean × 1.1.
   - The final-settle (three apps open) compositor CPU mean is above GNOME's × 1.1.
   - The overview response p50 or p95 is above GNOME's by more than one capture interval (about 150 ms).

   Start the workload thresholds as warnings until R1 lands, and make idle and overview response blocking immediately.
4. **Cheap per-PR guard.** Add a unit-level test that no overview transition frame requests a card render at a non-settled size. This PR adds that invariant via `WorkspaceCard::settled`. Add a nested-session smoke that counts rendered frames over 5 s of idle and fails if it exceeds a handful. The winit path always repaints, so that smoke needs the DRM path or a renderer-side counter.
5. **In-guest profile on demand.** Add an optional `perf record -g -p <compositor> -- sleep 20` during the final settle phase to the paired lane, retained as an artifact. This is how R1 and R5 should be confirmed.
