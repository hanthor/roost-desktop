---
created_date: "2026-10-08"
document_status: final
closed_date: "2026-10-08"
---

# Feature: 20261008122728-6ac4a932-gnome-animation-parity

## Overview

Tuna Desktop already looks like GNOME 51 when it is still, but it jumps from state to state. Windows appear, close, minimize and maximize with no motion. Workspaces switch instantly, and menus, popups and notifications pop in and out. This work gives Tuna Desktop the motion of GNOME 51: every animation GNOME plays, with the same timing and feel. It also follows GNOME's motion preferences, so turning on reduced motion keeps gentle fades instead of removing all feedback. The animations have to run more cheaply than GNOME's do, so people switching from GNOME get a desktop that feels the same and runs lighter.

## Requirements

- [ ] **Window open**
  When an ordinary window first appears, the system must animate it in the way GNOME 51 does: growing from a small size into place while fading in, with GNOME's duration and easing. Dialogs attached to a parent must unfold vertically instead.
- [ ] **Window close**
  When a window closes, the system must keep showing it long enough to play GNOME 51's closing animation (shrinking slightly while fading out; attached dialogs fold vertically), and the closing window must not take input during that time.
- [ ] **Minimize and restore**
  Minimizing a window must animate it towards its app's icon in the dash or dock, or towards the screen corner GNOME uses when there is no icon. Restoring it must reverse that motion, with GNOME's timing.
- [ ] **Maximize, unmaximize and tiling**
  Changing a window's size through maximize, unmaximize, half-tiling or fullscreen must transition smoothly from the old frame to the new one in the way GNOME 51 does, instead of jumping.
- [ ] **Workspace switching**
  Switching workspaces by keyboard must slide between them in the direction of travel. A touchpad workspace swipe must follow the user's fingers and finish or cancel with momentum matching the gesture.
- [ ] **Workspace indicators**
  The workspace switcher popup must fade in and out, and the top bar's workspace dots must resize with animation when the active workspace changes.
- [ ] **Dimming behind dialogs**
  When a modal dialog attaches to a window, the parent window must dim with animation, and it must brighten again when the dialog closes.
- [ ] **Overview transitions**
  Opening and closing the overview must use GNOME 51's separate easing for each direction. Moving between the window picker and the app grid, hovering a window preview, adding or removing workspace thumbnails, dragging a preview, showing search results and the top bar's change of background must all animate as they do in GNOME 51.
- [ ] **Overview gestures keep momentum**
  When the user releases a touchpad overview gesture, the remaining motion must carry the gesture's speed and finish with GNOME's timing instead of a fixed rate.
- [ ] **App grid**
  Opening and closing an app folder, app icons appearing, launching an app from the grid, and turning grid pages must animate with GNOME 51's motion and timing.
- [ ] **Menus and popups**
  Top-bar menus, quick-settings submenus and other shell popups must open and close with GNOME 51's fade-and-slide, and submenus must expand and collapse smoothly.
- [ ] **Transient shell surfaces**
  The volume and brightness popups, notification banners, the notification list, the Alt+Tab switcher and modal shell dialogs (including the background dimming behind them) must enter, update and leave with GNOME 51's animations.
- [ ] **Lock screen**
  The lock screen must slide in when the session locks, slide away when the user starts to unlock (following a drag or swipe when one is used), and settle back with GNOME's timing.
- [ ] **Desktop-level transitions**
  Wallpaper changes must crossfade. Taking a screenshot must flash the captured area. The hot corner must show GNOME's ripple when triggered. Session start and monitor configuration changes must use GNOME's transitions.
- [ ] **Honor enable-animations**
  When the user turns animations off, every animation above must complete instantly, and the end state must be identical to the animated end state.
- [ ] **Reduced motion keeps fades**
  When the user asks for reduced motion, animations must drop movement and scaling but keep fades and opacity changes, as GNOME 51 does, instead of turning all feedback off.
- [ ] **Animation speed setting**
  The system must honor GNOME's animation slow-down factor, so that animations can be uniformly slowed down for accessibility and debugging.
- [ ] **Preferences apply live**
  Changes to the animation, reduced-motion and slow-down settings must take effect for the next animation without restarting the session.
- [ ] **No interaction penalty**
  Input must never wait for an animation to finish. A new action interrupts or retargets the running animation from its current visual state without a visible jump.
- [ ] **Animations outperform GNOME**
  Running the same animation workload on the same machine, the system must use less CPU time than GNOME 51 and must hold frame pacing at the display's refresh rate at least as consistently as GNOME 51.
- [ ] **Animations are measurable**
  Every animation family must be observable and verifiable in automated tests, including its start, progress, settled state, and its behavior with animations turned off and with reduced motion.

## Constraints

- **GNOME Shell 51.0 is the behavioral reference.** Triggers, durations, easing and motion-preference semantics are taken from GNOME 51, the project's recorded parity baseline (decided 2026-10-01). Where this spec and GNOME 51 disagree, GNOME 51 wins, unless a recorded deviation says otherwise.
- **Performance must beat GNOME, not merely match it.** The user's direction is "aim for GNOME functionality but outperform it". An animation that matches GNOME visually but costs more CPU, or holds frame pacing worse than GNOME 51 on the same machine, does not meet this spec.
- **Original implementation only.** GNOME Shell and Mutter source may be studied for behavior. Their code must not be copied, per the project's reference-repository policy.
- **The pinned Smithay version stays.** The compositor's Smithay pin (ADR 0001) is not changed by this work. Anything Smithay lacks is built in the project's own code.
- **The compositor's critical path stays bounded.** Animations must never make input handling or frame submission wait on the shell, on extensions or on I/O. This is parent requirement R12.
- **Control protocol compatibility is preserved.** Any new information exchanged between compositor and shell follows the versioned control-protocol policy: a minor-version bump, plus explicit rejection of incompatible peers.
- **CI hardware is software-rendered.** Graphical proofs run on software rendering (llvmpipe) and virtual KMS, so every animation must work and be verifiable there, as well as on GPU hardware.
- **No parity claim without paired evidence.** An animation counts as ported only when an automated comparison with GNOME 51 capture evidence passes. This is the parity ledger rule.

## Acceptance Criteria

Timing criteria below compare against GNOME Shell 51.0's reference values. "Matches GNOME" means: frames sampled at fixed delays after the trigger fall within 3% of screen pixels of GNOME 51 frames captured at the same delays in the same scene, and the animation settles no earlier than GNOME's duration minus one frame and no later than GNOME's duration plus two frames at 60 Hz.

- [ ] **Window open matches GNOME**
  When a test window maps, sampled frames show it scaling up from below its centre and fading in, and it settles at its final geometry within 150 ms (+2 frames). An attached dialog unfolds vertically and settles within 100 ms (+2 frames). Both match GNOME.
- [ ] **Window close matches GNOME**
  After a test window closes, frames continue to show it shrinking and fading for 150 ms (+2 frames), or folding vertically for 100 ms for a dialog, before it disappears. Pointer clicks on its area during that time reach the window underneath, never the closing one.
- [ ] **Minimize and restore match GNOME**
  Minimizing a window that has a dash or dock icon shows it moving and shrinking towards that icon's on-screen rectangle and settling within 400 ms (+2 frames). With no icon, it heads towards GNOME's fallback corner. Restoring reverses the path. Frames match GNOME.
- [ ] **Size changes match GNOME**
  Maximize, unmaximize, half-tile left and right, and fullscreen each show intermediate frames between the old and new geometry, settle within 250 ms (+2 frames), and match GNOME. No frame shows the window's new content stretched beyond its final size.
- [ ] **Keyboard workspace switch slides**
  Super+Page Down and Super+Page Up show both workspaces sliding in the direction of travel and settle within 250 ms (+2 frames), matching GNOME.
- [ ] **Workspace swipe follows the fingers**
  During a scripted three- or four-finger horizontal swipe, the visible workspace offset tracks cumulative finger travel within 2% of screen width. A fast release finishes the switch sooner than a slow one, and a short release snaps back to the original workspace.
- [ ] **Workspace indicators animate**
  The workspace switcher popup fades in and out over 100 ms (+2 frames). The top bar's active workspace dot reaches its new size over 500 ms (+2 frames).
- [ ] **Parent dims behind modal dialog**
  When a modal dialog attaches, the parent's measured brightness falls to GNOME's dimmed level over 500 ms (+2 frames). It returns to full brightness over 250 ms (+2 frames) after the dialog closes.
- [ ] **Overview transitions match GNOME**
  Opening and closing the overview each take 250 ms (+2 frames), and mid-transition frames in each direction match GNOME's frames in that direction. Window picker to app grid, preview hover, thumbnail add and remove, preview drag start and revert, search results appearing and the top bar background change each show intermediate frames and match GNOME.
- [ ] **Overview gesture release keeps momentum**
  Releasing a scripted overview swipe at two different speeds from the same position produces two different completion times. The faster release finishes first.
- [ ] **App grid animations match GNOME**
  Opening and closing a folder shows it zooming from and back to its icon over 200 ms (+2 frames). Icons appearing scale in, launching from the grid shows the launch zoom, and page turns settle within 300 ms (+2 frames). All match GNOME.
- [ ] **Menus and popups animate**
  Opening the quick settings menu, the calendar and a header-bar menu shows a fade-and-slide that settles within 150 ms (+2 frames). Expanding a quick-settings submenu animates its height over 200 ms (+2 frames). All match GNOME.
- [ ] **Transient surfaces animate**
  The volume popup fades in and out over 100 ms. A notification banner enters over 200 ms with GNOME's slight overshoot and leaves over 200 ms. Alt+Tab fades out over 100 ms on release. Modal shell dialogs and their background dimming fade over 100 ms. Each is within +2 frames and matches GNOME.
- [ ] **Lock screen slides**
  Locking shows the lock screen sliding down into place. Starting to unlock slides it up over 300 ms (+2 frames), or follows a scripted drag and settles from the release point. Cancelling slides it back over 250 ms (+2 frames).
- [ ] **Desktop-level transitions**
  Changing the wallpaper produces at least one frame blending the old and new images, and the change completes in 1000 ms (+2 frames). A screenshot shows a white flash over the captured area. Triggering the hot corner shows ripples. Session start and adding or removing a monitor show GNOME's 500 ms transitions.
- [ ] **Animations off means instant**
  With enable-animations off, every scenario above reaches its final frame in the first presented frame after the trigger. That final frame is pixel-identical to the settled frame of the same scenario with animations on.
- [ ] **Reduced motion keeps fades only**
  With reduced motion on, window open, close, minimize, size change and workspace switch show only opacity changes: no frame shows the window displaced or scaled from its start or end geometry. At least one intermediate frame shows partial opacity. The volume popup and banners still fade.
- [ ] **Slow-down factor scales durations**
  With GNOME's slow-down factor set to 4, the window open animation settles in 600 ms (±2 frames). Every other sampled animation scales by the same factor.
- [ ] **Live preference changes**
  Toggling enable-animations, reduced motion or the slow-down factor in GNOME Settings or gsettings changes the very next animation's behavior with no session restart.
- [ ] **Input never waits**
  Pressing Super+Page Down twice within 50 ms ends on the second workspace over, with no frame jumping backwards. Typing into a window that is still animating open delivers every keystroke. Minimizing a window mid-open retargets smoothly: no frame shows it snapping to full size first.
- [ ] **Cheaper than GNOME**
  On the same Marlin VM and the same scripted animation workload (open, close, minimize, maximize, workspace switch and overview cycles, repeated), Tuna Desktop's median CPU time is below GNOME 51's. Its share of on-time frames during animations is at least GNOME 51's. Both are recorded in a committed, reproducible performance report.
- [ ] **Every family has an automated proof**
  CI runs a graphical proof for each animation family above. Each proof asserts start, progress, settle time, the animations-off result and the reduced-motion result, and it fails when any of them regresses.

## Technical Approach

Non-binding direction. The plan workflow may adopt, adapt or replace any of it.

- **Window, workspace, minimize, size-change and dimming animations belong in the compositor as render-time transforms.** Animating opacity, scale and position while drawing avoids reconfiguring clients and repainting the shell, which is the main lever for beating GNOME on CPU.
- **Close and size-change animations need the old frame.** A closing or resizing window's last frame should be kept as a texture and animated, since its client has already let go of it or replaced it.
- **Treat shell-side transient surfaces generically where possible.** Popups, OSD, banners, the switcher and dialogs are layer or popup surfaces. One compositor-side enter/leave animation for them may beat separate per-widget GTK animations. Use per-widget GTK/libadwaita animation only where the motion lives inside a surface: submenu expansion, folder zoom, message list items.
- **One motion policy everywhere.** Replace the current single on/off flag with one shared policy, derived from GNOME's enable-animations, reduced-motion and slow-down settings, that both the compositor and the shell read. Every duration then passes through one adjustment, as GNOME does.
- **A deterministic animation clock for tests.** Provide a way for proofs to drive time, so CI can sample exact delays on software rendering without flakiness.
- **Reuse the existing proof pattern.** Easing curves as pure functions with unit tests, animation progress published in compositor state, and graphical journey gates that assert start, settle, animations off and reduced motion. Extend the GNOME reference capture harness to record GNOME 51 frames at the same fixed delays.
- **Build on what is already there.** Overview transitions, tile preview, notification group expansion, idle fade, the unlock crossfade and the scroll-strip springs already animate. Correct their timing and easing where the audit found differences rather than rewriting them.
- **Source of reference values:** the GNOME Shell 51.0 checkout in the project's reference directory. The 2026-10-08 audit maps each of the 54 animations to its GNOME source, duration and easing, and to Tuna Desktop's current state.
- **Suggested delivery order, by user visibility:** motion policy and test clock; window open and close; minimize and restore; size changes; workspace slide and swipe; parent dimming; overview corrections; app grid; menus and transient surfaces; lock screen and desktop-level transitions.
- **Risk:** frame pacing on software rendering may limit what CI can prove about performance. Treat the "cheaper than GNOME" check as a Marlin VM measurement (KVM, same image), not a nested CI result.

## Success Metrics

- Animation coverage: 50 of 50 GNOME 51 animations whose features exist in Tuna Desktop are ported, with a passing paired comparison. The baseline on 2026-10-08 was 6 ported and 7 partial. The 4 animations tied to absent features (on-screen keyboard, magnifier, app-not-responding dialog, pointer accessibility) are tracked against those features.
- Visual fidelity: every sampled mid-animation frame is within 3% of screen pixels of the GNOME 51 frame at the same delay. Settle times are within GNOME's duration −1/+2 frames at 60 Hz.
- CPU: on the same Marlin VM and the same scripted animation workload, Tuna Desktop's median CPU time is below GNOME 51's. The 2026-10-04 whole-run baseline was 159% of a core for Roost versus 19% for GNOME, so this measure also tracks the wider efficiency gap.
- Frame pacing: during animations, the share of frames presented on time is at least GNOME 51's on the same VM. Target: 99% or more of frames on time at 60 Hz.
- Responsiveness: overview open visible-response median at or below GNOME 51's. The 2026-10-04 baseline was 382 ms for Roost versus 159 ms for GNOME.
- Accessibility semantics: with reduced motion on, 0 sampled frames show motion or scaling in the motion-dropping families, and fades remain. With animations off, 100% of scenarios are pixel-identical to their settled animated state.
- Regression safety: each animation family has a CI gate. Zero animation gates are red on main for 14 consecutive days after delivery.
- Parity ledger: an animation row exists for each family, and each row reads `pass` with a linked test ID.

## Non-Goals

Drafted and accepted under the user's delegation ("dont ask me just make it as you see best").

- Building the on-screen keyboard, magnifier, app-not-responding dialog or pointer accessibility features is out of scope. Their GNOME animations arrive with those features, which are tracked in their own issues (#349, #350).
- GNOME Shell extension animation APIs, and any compatibility with GNOME Shell's JavaScript or Clutter animation interfaces, are out of scope.
- New animations GNOME 51 does not have are out of scope. The existing scroll-strip springs (a Tuna Desktop feature with no GNOME equivalent) stay as they are and are not removed for parity.
- Making application client-side animations (inside GTK or Qt apps) match GNOME is out of scope. This spec covers motion drawn by the compositor and the shell only.
- Closing the wider CPU efficiency gap outside animations is out of scope here. It is tracked by the performance baseline work (#73, #315), although this spec's CPU metric must still be met.
- Variable-refresh-rate and high-refresh tuning beyond holding the display's refresh rate is out of scope.
