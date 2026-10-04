# Marlin VM lifecycle proof

The existing `marlin-vm` CI job installs the ordinary Marlin preview image,
then a CI-only fixture layer. The fixture starts `roost-test` once through
greetd's initial session. Its default greeter is Cage with normal-window
gtkgreet, matching the measured TunaOS launcher; logout must reach that
actual greeter. This changes neither the shipped preview image nor the
number of CI VM profiles.
The CI session wrapper routes its output through `systemd-cat` so greetd's
VT output reaches the fixture's journal-to-serial capture.

The Python lane.boot API keeps its original four-value return and original
VM device profile by default. Lifecycle callers explicitly request
`guest_agent=True` to add the QGA transport and receive its socket as a fifth
value; existing performance callers keep working without a guest agent.

QEMU's guest agent executes a fixed probe with a root UID, while the
compositor, shell and application clients must belong to the non-root
session owner. Artifacts record window IDs, app IDs, rectangles, process
PIDs/start ticks and redacted session metadata. They contain no process
arguments, passwords or application titles.

The last of the existing five pristine boots runs these checks after the
feature tour:

| Gate | Actual operation and evidence |
| --- | --- |
| V-LIFECYCLE-OWNER | Root probe, non-root greetd session, three application surfaces/processes |
| V-NORMAL-INPUT / V-NORMAL-INPUT-FAIL-CLOSED | A real GTK client receives ordinary QMP keys while unlocked, then receives none while locked; artifacts contain only counts and PID, never key values |
| V-VT | VT away/back, DRM pause/activate events, unchanged process identities and app rectangles, matching body plus a fresh visible count update from the real GTK client |
| V-SUSPEND | Real logind suspend reaches QEMU `suspended`; the first guest snapshot after `system_wakeup` must already be locked, with all application identities intact; the real varied lock mask must repaint within five seconds, matching the pre-suspend mask; QEMU inactive-output placeholders fail |
| V-SLEEP-AUTH-FAIL-CLOSED / V-SLEEP-FRESH-AUTH | A second real suspend interrupts a correct password attempt delayed by actual pam_exec; the real PAM success from the old generation is refused, then fresh correct authentication succeeds |
| V-SLEEP-CLIENT-REPAINT | The same GTK client receives a fresh key and visibly repaints after wake/unlock |
| V-VT-FAIL-CLOSED | VT away/back while locked keeps the mask and original application processes |
| V-AUTH-FAIL-CLOSED | Real PAM service temporarily uses `pam_deny`; fresh submitted/refused milestones prove the correct test password was attempted and denied; original service restored afterward |
| V-CAPTURE-INPUT-FAIL-CLOSED | Reachable real untrusted grim/wtype clients are denied capture/virtual-keyboard interfaces while locked; a successful Wayland connection is required and timeouts do not count as refusal; this does not assert that these interfaces are available while unlocked |
| V-SHELL-FAIL-CLOSED | Stop the portal and kill the lock UI; original app/compositor identities survive and the supervised replacement stays locked |
| V-LOGOUT | Terminate the real logind session; active greetd has gtkgreet and no original compositor |

The original tour's `V-LOCK` still requires exactly unlocked → locked →
unlocked. Its observations end before lifecycle operations add their own
lock transitions. A QEMU pause is never a substitute for guest suspend.
The repaint comparison excludes the panel clock and tolerates up to 20%
changed body pixels for applications such as System Monitor; it also
requires exact window and process identity preservation.

The compositor also listens to the trusted logind system-bus sleep signal:
preparation invalidates pending authentication generations and engages its own lock; wake restores KMS connector/plane
state and retires lost flip buffers without marking them presented. Wake
recovery does not depend on a VT change; an off-seat wake waits for seat
activation. This does not establish a delay inhibitor or physical masked
frame acknowledgement before sleep.

These new gates require their own actual CI results before acceptance.
Output hotplug and loss of every compositor-adjacent service remain
unproven, as do physical hardware and 24-hour soak. This document does not
claim those parts of issue #62 are complete.

Protocol references: [QEMU guest agent](https://www.qemu.org/docs/master/interop/qemu-ga-ref.html)
and [QEMU wake-up](https://www.qemu.org/docs/master/interop/qemu-qmp-ref.html#command-system-wakeup).
