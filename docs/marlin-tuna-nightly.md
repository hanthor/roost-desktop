# Published Marlin Tuna Desktop nightly

The `marlin-tuna-nightly` workflow resolves the published TunaOS Marlin
flavor <!-- tuna-rename: keep -->(`ghcr.io/tuna-os/marlin:roost` until the `tuna` flavor is published)
to an immutable digest and installs a CI fixture layer on that published
flavor. It does not rebuild or replace Tuna Desktop. The fixture requires every
shipped package version to remain present unchanged, and checks hashes of
the session, compositor, shell, shipped Cage/gtkgreet wrapper, greetd
configuration, CSS and session environments before and after setup.

The fixture provisions the public `tuna-test` account and adds QEMU guest
agent probes, a count-only GTK input client, Disks and System Monitor for
the existing application tour, plus serial capture. A test-user profile
exports the introspection path and directs session output to the journal.
It preserves the selected `tuna-session` command and the shipped greeter
wrapper and styling. An initial/autologin session is rejected.

<!-- tuna-rename: keep-begin -->
Until the `tuna-desktop` package reaches the published flavor, that image
still ships the package from before the rename (#505), with `roost-`
prefixed binaries, session entry, PAM service and environment variables.
The fixture, probes and profile detect which identity the guest ships and
check that one. The current-source `marlin-vm` lane installs
`tuna-desktop`, so it always checks the new names.
<!-- tuna-rename: keep-end -->

Each of five pristine snapshot boots waits for the actual non-root
gtkgreet process, types the fixture username and password through QMP,
and requires a real non-root logind session with `Service=greetd` and the
actual Tuna Desktop compositor. The last boot runs the existing semantic tour
and [lifecycle checks](vm-lifecycle.md), including strict first-resume
locked state, real PAM refusal, VT repaint and logout. No pause substitute
or fabricated greeter success is accepted.

The VM profile is the existing q35/OVMF KVM guest: four CPUs, 6 GiB RAM,
virtio VGA at 1280×800 and the existing 20 GiB raw disk. The daily workflow
requires KVM rather than silently falling back to emulation. It also runs
on its focused pull request so the first published-image qualification is
reviewable before the schedule lands.

The artifact records the published digest, fixture image identity, shipped
package version and hashes, harness commit, run ID, frames, tour clip,
serial journal and redacted introspection. Passwords, process arguments
and input key values are excluded from probe artifacts. A failure writes
`failure.json` with the run URL, immutable image digest and a bounded
journal excerpt containing only lifecycle/health markers; it is retained
with the other evidence instead of sending messages or filing issues.

The workflow must produce an actual green published-flavor run before
issue #68 is accepted. These checks do not qualify physical hardware,
output hotplug or a 24-hour soak.

Source references: [shipped TunaOS greeter wrapper](https://github.com/tuna-os/tunaOS/blob/main/build_scripts/desktop/greetd-gtkgreet.sh),
[greetd profile/session execution](https://github.com/kennylevinsen/greetd/blob/master/greetd/src/session/worker.rs),
and [gtkgreet entry focus and activation](https://github.com/kennylevinsen/gtkgreet/blob/master/gtkgreet/window.c).
