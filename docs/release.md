# Cutting a Roost release

Supported scope: current Debian stable and Ubuntu LTS, amd64.

## Checklist

1. Land everything for the release on `main` with green CI.
2. Draft the release notes below (changes, known limitations, supported
   configurations).
3. Tag the release: `git tag vX.Y.Z && git push origin vX.Y.Z`.
4. From a clean checkout of the tag, build the package:
   `scripts/roost-release` — writes `dist/roost_X.Y.Z_amd64.deb`.
   Use `--allow-dirty` only for throwaway dev builds, never for a release.
5. Verify the artifact: install it on a clean supported system without
   dependencies pre-installed, confirm the login screen lists Roost, and
   confirm every binary's `--version` matches the tag.
6. Capture the demo media from the same tag:
   `scripts/roost-capture --artifacts walkthrough-media` — records the
   six-feature core tour (clip plus still per segment) and checks the
   segment/template contract. The capture is hermetic (private bus,
   probe home, isolated shell state), so a rerun on an unchanged
   feature must reproduce the previous release's shots: diff the new
   stills against the prior release's before uploading.
7. Attach the `.deb`, the notes, and the twelve media files
   (`<segment>.mp4` + `<segment>.png` per segment) to the release,
   e.g. `gh release upload vX.Y.Z walkthrough-media/*.mp4
   walkthrough-media/*.png`.
8. Render the walkthrough page:
   `scripts/roost-capture --artifacts walkthrough-media --render-walkthrough vX.Y.Z`
   writes `docs/walkthrough.md` with versioned release-asset links;
   commit it so the docs site shows the new tour.

The package version, the binaries' `--version` output, and the notes must
agree — the release script enforces the first two by construction.

## Upgrade and uninstall

Upgrading installs the newer package over the older one; user configuration
and application data are preserved. Uninstalling removes the Roost-owned
program files and the session entry; user configuration and application data
are preserved.

## Release notes template

```markdown
# Roost X.Y.Z

## Changes
- ...

## Known limitations
- ...

## Supported configurations
- Debian ... / Ubuntu ... (amd64)
```
