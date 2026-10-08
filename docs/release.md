# Cutting a Tuna Desktop release

Target: TunaOS Marlin (Arch-based bootc, x86_64), with GNOME 51 as the comparison baseline ([ADR 0007](adr/0007-gnome51-marlin-baseline.md)). The project remains a developer preview until the roadmap release gates pass. Debian/Ubuntu production support must include the preferred GTK shell and
actual installed-session control qualification. The first package lane targets
Ubuntu 26.04 amd64; Debian 13 still needs a packaged gtk4-layer-shell >=1.1
backport and a separately built/qualified artifact. These package targets do
not provide Marlin or physical hardware qualification.

## Checklist

1. Land everything for the release on `main` with green CI.
2. Fill the visual review: copy `docs/reviews/TEMPLATE.md` to
   `docs/reviews/vX.Y.Z.md`, compare the CI proof frames against the
   GNOME 51 baseline bundle, and commit it with no `TBD` left. The
   release script refuses a tagged build without it.
3. Draft the release notes below (changes, known limitations, supported
   configurations).
4. Tag the release: `git tag vX.Y.Z && git push origin vX.Y.Z`.
5. From a clean checkout of the tag, build the package:
   `scripts/tuna-release` — writes `dist/tuna-desktop_X.Y.Z_amd64.deb`.
   Use `--allow-dirty` only for throwaway dev builds, never for a release.
6. Run `scripts/check-release-package` on the artifact. Require all six
   binaries, complete version agreement and the generated ELF dependency
   closure plus explicit runtime floors. Install it on the selected target
   with apt, retaining package/hash/distro dependency receipts and the
   installed GTK control proof; an extracted/debug binary is insufficient.
   Verify the artifact: install it on a clean supported system without
   dependencies pre-installed, confirm the login screen lists Tuna Desktop, and
   confirm every binary's `--version` matches the tag.
7. Capture the demo media from the same tag:
   `scripts/tuna-capture --artifacts walkthrough-media` — records the
   six-feature core tour (clip plus still per segment) and checks the
   segment/template contract. The capture is hermetic (private bus,
   probe home, isolated shell state), so a rerun on an unchanged
   feature must reproduce the previous release's shots: diff the new
   stills against the prior release's before uploading.
8. Attach the `.deb`, the notes, and the twelve media files
   (`<segment>.mp4` + `<segment>.png` per segment) to the release,
   e.g. `gh release upload vX.Y.Z walkthrough-media/*.mp4
   walkthrough-media/*.png`.
9. Render the walkthrough page:
   `scripts/tuna-capture --artifacts walkthrough-media --render-walkthrough vX.Y.Z`
   writes `docs/walkthrough.md` with versioned release-asset links;
   commit it so the docs site shows the new tour.

The package version, the binaries' `--version` output, and the notes must
agree — the release script enforces the first two by construction.

## Upgrade and uninstall

Upgrading installs the newer package over the older one; user configuration
and application data are preserved. Uninstalling removes the Tuna Desktop-owned
program files and the session entry; user configuration and application data
are preserved.

## Release notes template

```markdown
# Tuna Desktop X.Y.Z

## Changes
- ...

## Known limitations
- ...

## Supported configurations
- Debian ... / Ubuntu ... (amd64)
```
