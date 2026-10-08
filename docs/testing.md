# Running tests locally

How to run, on your own machine, the checks the `ci` workflow
(`.github/workflows/ci.yml`) runs on every pull request. For what each
verification layer must prove, see [the test strategy](test-strategy.md).
Install the packages listed under [Prerequisites](development.md#prerequisites)
first; each proof below names anything extra it needs.

## Before opening a PR

The `check` job is the gate every other job waits on. Run its four steps
in this order:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Clippy runs with `-D warnings`, so any warning fails CI.

`cargo test --workspace` includes the 100-run shell fault harness
(`hundred_fault_runs_keep_apps_alive_and_resync` in
`crates/compositor/tests/recovery.rs`) and needs no display or GPU.

The lock-screen PAM test (`crates/compositor/tests/pam_unlock.rs`) needs
`pam_wrapper` (`libpam-wrapper` on Ubuntu). Without it the test prints
`pam_unlock: pam_wrapper not installed, skipped` and passes. CI sets
`ROOST_REQUIRE_PAM_WRAPPER=1`, which turns that skip into a failure; set it
locally to be sure the test really ran:

```sh
ROOST_REQUIRE_PAM_WRAPPER=1 cargo test --workspace
```

Run one crate with `cargo test -p <package>` (package names are in each
crate's `README.md`, for example `cargo test -p roost-compositor`).

## Nested proofs

Each proof job builds the workspace, runs the nested compositor on a
private Xvfb (unless `DISPLAY` already works), drives it with real X input,
and writes frames, logs, and usually an `assertions.txt` into the
artifacts directory. A nonzero exit is a failure; the frames are for human
review. CI shellchecks each script before running it.

| CI job | Command | Extra packages | What it asserts |
|---|---|---|---|
| `journey` | `scripts/roost-journey --artifacts journey-artifacts` | `xvfb xdotool x11-apps scrot jq dbus`, Mesa EGL (`libegl1 libgl1-mesa-dri`) | Overview, search, launch from search, Escape, workspace switch, Alt+Tab; writes `assertions.txt` |
| `journey` | `scripts/roost-release --allow-dirty --out package-artifacts && scripts/check-release-package package-artifacts/*.deb` | as above | The Debian package builds and passes its checks |
| `app-content` | `scripts/roost-app-content --artifacts app-content-artifacts` | `python3-venv`; the script installs Playwright and Chromium itself | Chromium maps as a Wayland client, tiles with Super+Left, closes with Alt+F4 |
| `scroll-proof` | `scripts/roost-scroll-proof --artifacts scroll-proof-artifacts` | `python3-venv` | Scroll-mode strip: Super+Shift+T, Super+R, wheel scrolling, gap geometry |
| `gtk-shell` | `ROOST_REQUIRE_PAM_WRAPPER=1 scripts/roost-gtk-shell-proof --artifacts gtk-shell-artifacts` | gtk4-layer-shell (`scripts/ci-install-gtk4-layer-shell`), `xwayland pipewire wireplumber at-spi2-core python3-pyatspi fonts-cantarell ffmpeg libpam-wrapper` and the GStreamer PipeWire plugins; see the job for the full list | GTK shell panel, calendar, quick settings, overview, notifications, lock and unlock, AT-SPI trees; writes `assertions.txt` |
| `gtk-shell` | `scripts/roost-scale-proof --artifacts gtk-shell-artifacts/scale` | as above | The same session at 150 percent scale; writes `assertions.txt` |
| `drm-smoke` | `scripts/roost-drm-smoke --artifacts drm-smoke-artifacts` | `seatd jq ffmpeg`, `linux-modules-extra-$(uname -r)` for vkms | Hardware backend on the vkms virtual KMS device; needs `sudo` for `modprobe` and seatd |

`app-content` and `scroll-proof` download Playwright and Chromium into a
temporary venv on every run, so they need network access.

**Start `roost-gtk-shell-proof` from a fresh artifacts directory.** The
script points `XDG_STATE_HOME` at `gtk-shell-artifacts/state`, and the
shell keeps its state, notification history included, under
`$XDG_STATE_HOME/roost-shell`. A reused directory carries the previous
run's history into the next one, so the notification stages no longer
start from an empty list. Remove it first:

```sh
rm -rf gtk-shell-artifacts
ROOST_REQUIRE_PAM_WRAPPER=1 scripts/roost-gtk-shell-proof --artifacts gtk-shell-artifacts
```

`scripts/roost-drm-smoke` loads a kernel module and starts seatd as root.
Run it in a VM or a disposable machine rather than your desktop session.

## Marlin VM lane

The `marlin-vm` job (#68) boots the real TunaOS Marlin image with Tuna Desktop
installed and checks that the display manager logs into the Tuna Desktop
hardware session. A CI-only guest service probes the calendar and portal
on the test user bus; the runner reads the serial log and takes QMP
screendumps from outside.

1. It builds `packaging/marlin/Containerfile` (Marlin GNOME 51 plus the
   Tuna Desktop Arch package from `arch-package`) with rootful podman. On top of
   that it builds the CI-only layer `packaging/marlin/vm-lane/Containerfile`.
   This layer adds a `roost-test` user and turns on GDM automatic login
   into "Tuna Desktop (preview)". It also sends the kernel console and the
   journal to `ttyS0`. The shipped image does not get these changes.
2. It runs `bootc install to-disk --via-loopback --generic-image` from
   that image to write a 20 GB raw disk.
3. `scripts/roost-vm-lane --disk disk.raw --out vm-lane-artifacts --boots 5` boots
   the disk in QEMU/KVM with OVMF firmware and virtio-vga at 1280x800.
   It writes these assertions to `assertions.txt`:

| ID | What it asserts |
|---|---|
| `V-DRM` | The serial journal has `roost-compositor: drm: output ...`, so the compositor lit an output on the DRM/KMS backend |
| `V-SHELL` | `roost-shell-gtk: keybindings:` appears and shell supervision never logs `BudgetExhausted` |
| `V-PANEL` | The session opens on the overview, where the panel is transparent. After Escape, a screendump shows a pure black strip at y=5 across at least 90 percent of the width (the top panel) over a desktop that is not one flat color |
| `V-NOPANIC` | No `panicked` anywhere in the serial log |

The lane retains plymouth and makes five pristine snapshot boots. Tuna Desktop
explicitly registers its ready session and display with GDM instead of
relying on GDM's delayed fallback. Every boot must retain DRM ownership
and show the panel. Earlier tests disabled plymouth after a greeter stole
DRM master about ten seconds after autologin; the repeated gate now tests
that startup path directly.

Additional gates are `V-GDM` (explicit registration, no session pause),
`V-CALENDAR` (CalendarServer responds), `V-PORTAL` (portal Settings responds),
`V-LOCK` (the final tour observes unlocked → locked → unlocked), and
`V-REPEAT` (all five boots pass). Earlier boots keep their own artifacts
under `repeat-NN/`; `repeat-manifest.json` records every boot, while the
root `assertions.txt` aggregates results for the parity ledger.

The artifact (`marlin-vm`) holds `serial.log`, `roost-lines.log` (every
`roost-*` line), the boot frames, `V-OVERVIEW.png` (the login overview),
`V-SESSION.png` (the desktop), and `manifest.json`.
The manifest records the base image digest, the test image ID, the Tuna Desktop
package, and the QEMU version. If the run fails or times out (15
minutes), the script prints the relevant serial lines before exiting.

After the assertions pass, `--tour` records a feature tour. The script
sends keys and pointer clicks through QMP to open the overview and search,
the app grid, quick settings, and the calendar. It then opens Disks,
System Monitor, and Files, switches between them with Alt+Tab, and turns
scroll mode on and off (Super+Shift+T, Super+R, Super+Left/Right). Last,
it locks the screen with Super+L and unlocks it with the `roost-test`
password through PAM. Only the last repeated boot records the tour. The
script takes a screendump twice a second and plays the result back at
double speed. Before each section it inserts a title card, a committed
PNG in `scripts/lib/vm-tour-cards/` (`render.py` there redraws them). The
`roost-tour` artifact, kept for 30 days, holds `roost-tour.webm`,
`preview.webp`, and one `T-<section>.png` still per section. The tour
asserts nothing; only a missing video fails the job.

You need `/dev/kvm`, about 30 GB of free disk, and the podman and
`bootc install` commands from the job to run this locally. Do not use your
desktop session for it.

## Animation clock and motion policy

Compositor animations (the overview transition, the scroll-strip springs and
the tile preview) read time from one animation clock
(`crates/compositor/src/animation_clock.rs`). Two environment variables
make their timing reproducible on slow CI renderers such as llvmpipe:

| Variable | Read by | Effect |
|---|---|---|
| `ROOST_ANIMATION_CLOCK` | compositor, only when `ROOST_COMPOSITOR_STATE` is also set | Absolute path of a clock file. While the file is absent, animations run on real time. When it first holds a whole number of milliseconds `N`, animation time freezes at that moment plus `N`. Rewriting it with `M` moves time to that moment plus `M`. Removing it resumes real time. Time never runs backwards across these switches. |
| `GNOME_SHELL_SLOWDOWN_FACTOR` | shell (sent to the compositor in `InputSettings`) | GNOME Shell's own slow-down factor: a positive number multiplies every animation duration, as GNOME's `adjustAnimationTime` does. Anything else keeps 1. Read at shell start. |

Write the clock file atomically (write a temporary file, then `mv`), as
`anim_clock` in `scripts/roost-gtk-shell-proof` does. The compositor state
file reports `animation_clock_manual` and, while the clock is frozen, `animation_clock_ms`, plus the
effective `motion_policy` (`full`, `fade-only` or `off`),
`animation_slowdown` and `animations_enabled` (GNOME's
`enable-animations`, which Reduced Motion leaves on). For example, the
overview transition takes 250 ms each way, so after Super:

```sh
echo 0 > clock.tmp && mv clock.tmp "$ROOST_ANIMATION_CLOCK"    # freeze
xdotool key Super_L                                            # progress 0
echo 125 > clock.tmp && mv clock.tmp "$ROOST_ANIMATION_CLOCK"  # progress 0.5
rm "$ROOST_ANIMATION_CLOCK"                                    # real time again
```

Proof `G-ANIMATION-CLOCK` samples the overview this way at 125 and 250 ms
in both directions. `G-ANIMATIONS-OFF` and `G-INTROSPECT-MOTION` check the
live motion policy: animations off snaps everything, Reduced Motion
(fade-only) snaps overview and strip motion but keeps the idle-shield fade.

## Parity ledger check

The `parity-ledger` job runs `scripts/roost-ledger` against the test list
from `check` and the `assertions.txt` files from the proofs. Reproduce it
after running the proofs above:

```sh
cargo test --workspace -q -- --list --format terse > test-list.txt
scripts/roost-ledger check --tests test-list.txt \
  --assertions journey-artifacts/assertions.txt \
  --assertions gtk-shell-artifacts/assertions.txt \
  --assertions gtk-shell-artifacts/scale/assertions.txt \
  --assertions drm-smoke-artifacts/assertions.txt \
  --assertions vm-lane-artifacts/assertions.txt
```

`scripts/roost-ledger summary` prints status counts only.

The rule, from [the parity ledger](parity-ledger.md): a row is `pass` only
when a test or recorded review compares Tuna Desktop against GNOME 51 baseline
evidence, and `untested` is not a pass. The check fails when a row whose
status starts with `pass` cites no test, when a cited `cargo:` test does
not exist in the test list, when a cited `journey:` or `proof:` assertion
is not recorded as passing, or when the table is malformed or repeats an
ID. If your change renames or removes a test, update every ledger row that
cites it in the same PR.

## Release screenshots

The `docs-shots` job runs `scripts/roost-docs-shots collect` over the
`journey`, `app-content` and `scroll-proof` artifacts and uploads the
curated frames as the `docs-shots` artifact. Nothing in the repository is
refreshed from it. The docs show the GTK shell through the
[walkthrough](walkthrough.md), which the GTK shell proof captures.

## Pixel parity capture (not in CI)

`scripts/roost-parity-capture` captures Tuna Desktop in the states
`scripts/roost-gnome-reference` captures GNOME 51 in, for comparison with
`scripts/lib/roost-parity-compare.py`. CI does not run it. The workflow and
the states are in [pixel parity with GNOME 51](gnome-parity.md).

## When a run fails

Read `nested.log` in the artifacts directory first (`compositor.log` for
`roost-drm-smoke`); see
[nested session troubleshooting](nested-session.md#troubleshooting). Do not
retry a deterministic test until it passes: the timing rule in the
[development guide](development.md#writing-a-test) applies.

## Bounded nested stress

`nested stress` runs four concurrent Wayland GTK clients. Each round maps
and destroys 400 windows; three warmup rounds settle renderer caches.
After every measured round, Escape and Super must still change the
compositor's overview state, and every destroyed window must disappear.
The lane then sends 1,000 notifications and closes each over D-Bus.

The per-change smoke uses ten measured rounds; nightly uses 300. The
report retains PSS/RSS, file descriptors and descendant process counts
for the compositor and supervised shell after each round. Final PSS must
stay within warmup PSS × 1.25 + 64 MiB, and fd count within warmup + 32.
These diagnostic bounds catch unbounded growth in this workload; they
are separate from the GNOME performance targets in #73. A panic, lost
input response, orphaned window or hung worker fails the lane. Logs,
metrics, final state, assertions and the exact source revision are kept.

Run it on a development host after building the compositor and GTK shell:

```sh
scripts/roost-stress --rounds 10 --out /tmp/roost-stress-fresh
```

Use a fresh empty artifact directory for each run. The harness starts its
own Xvfb, session bus and private user state. It covers client churn and
notification flood portions of #203; malformed Wayland requests, service
faults, hotplug, locked-input adversaries and a measured 24-hour VM soak
remain open.

## Control decoder fuzzing

The `control decoder fuzz` workflow exercises both arbitrary length-prefixed
frames and mutated postcard bodies with a generated valid length prefix.
Successful decodes must re-encode into another valid frame. Seed inputs
include a Hello, a truncated varint, and an oversized declared frame.

Pull requests touching the decoder or fuzz harness run each target for
60 seconds; the nightly lane runs each for 600 seconds. The address
sanitizer, five-second per-input timeout and 2 GiB RSS limit turn crashes,
hangs and runaway allocation into failures. Logs, the resulting corpus,
and crash artifacts are retained for 14 days. A found crash needs a small
regression test in the protocol crate before its fix is merged.

To reproduce locally, install the pinned tool and toolchain, then run:

```sh
rustup toolchain install nightly-2026-08-28
cargo install cargo-fuzz --version 0.13.2 --locked
cargo +nightly-2026-08-28 fuzz run control_body -- -max_total_time=60 -max_len=1048580 -timeout=5 -rss_limit_mb=2048
```

This covers the decoder portion of #203. Wayland misuse, client churn,
service failures and a measured long soak remain separate stress gates.
