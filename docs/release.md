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
6. Attach the `.deb` and the notes to the release.

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
