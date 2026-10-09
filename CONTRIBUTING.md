# Contributing to Tuna Desktop

Tuna Desktop is in design-and-planning stage: no compositor implementation has
started yet (see [README.md](README.md)). Most contributions right now are
planning artifacts (specs, plans, ADRs) and documentation. This guide covers
both that work and the code contribution process for when implementation
begins.

## Before you start

Read [AGENTS.md](AGENTS.md) first if you are an AI agent or are using one —
it defines where the code actually lives (`spektacular repo list`), how the
Spektacular-managed stores (`.spektacular/specs/`, `.spektacular/plans/`) must
be read and written, and the knowledge-base rules. Human contributors should
skim it too; the store-access rules bind everyone who touches those paths,
not just agents.

Also read:

- [docs/architecture.md](docs/architecture.md) — process/authority boundaries
  between compositor, shell host, and extensions.
- [docs/roadmap.md](docs/roadmap.md) — delivery sequence, spek registry, and
  requirement traceability.
- [docs/adr/README.md](docs/adr/README.md) — open and decided architecture
  decisions. A "blocking" decision must resolve before its dependent spek's
  work passes its gate.

## The Spektacular workflow

Specs and plans are managed through the [Spektacular](https://github.com/hivecommons/spektacular)
CLI, not by hand-editing files under `.spektacular/`. Install it if needed:

```sh
go install github.com/hivecommons/spektacular@latest
```

Ensure `$(go env GOPATH)/bin` is on `PATH`, then confirm it's working:

```sh
spektacular version check
```

Each program unit (a "spek") has a spec, a draft implementation plan,
context, and research artifacts. The lifecycle is:

1. **Spec** — describes the feature or requirement. See the spek registry in
   [docs/roadmap.md](docs/roadmap.md#spek-and-plan-registry) for the current
   list.
2. **Plan** — a draft reviewed against current source, confirmed
   dependencies, and explicit acceptance evidence before its implementation
   workflow starts. Do not begin implementation from a draft plan while a
   blocking decision or security boundary is unresolved
   ([docs/roadmap.md](docs/roadmap.md)).
3. **Implement** — starts from the corresponding full plan name after
   approval.

To start a plan for an existing spec, use the spec's full timestamp-prefixed
name from `spektacular spec file list`:

```sh
spektacular plan new --data '{"name":"<full-spek-name>"}'
```

Proposing a new spec or a change to the roadmap/architecture starts the same
way: describe the change, and if it's spec-worthy (multiple requirements, a
scoped decision, a buildable feature description), open the discussion as a
spec via `spektacular spec new` rather than editing `docs/roadmap.md` or
`docs/architecture.md` directly. Architecture *decisions* go through an ADR
in `docs/adr/` instead — see the existing ADRs there for the expected
format (status, context, decision, alternatives, consequences).

## Code contributions

Once a plan is approved and implementation starts:

- **Sign off every commit.** This project uses the Developer Certificate of
  Origin. Commit with `git commit -s` (or add `Signed-off-by:` by hand) —
  every commit in this repository's history carries it.
- **CI must pass.** [.github/workflows/ci.yml](.github/workflows/ci.yml) runs,
  on every push and pull request:
  - `cargo fmt --all -- --check`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo test --workspace`
  - a nested-session journey test (`scripts/tuna-journey`) that launches the
    nested compositor under Xvfb and asserts visible reaction; artifacts are
    uploaded for human review.
  - an app-content journey (headed Chromium via Playwright as a Wayland
    client) asserting the compositor renders real app content.

  Run the fast checks locally before pushing:

  ```sh
  cargo fmt --all -- --check
  cargo clippy --workspace --all-targets -- -D warnings
  cargo test --workspace
  ```

  The journey jobs need system packages (`libxkbcommon-dev`,
  `libgtk-4-dev`, `libadwaita-1-dev`, Xvfb, etc.) — see the `journey` and
  `app-content` jobs in `ci.yml` for the exact list per platform; there's no
  single local equivalent yet, so CI is authoritative for those.
- **Update docs and ADRs alongside code.** If a change affects a decision
  recorded in `docs/adr/`, or the delivery tiers/requirement map in
  `docs/architecture.md` and `docs/roadmap.md`, update them in the same PR.
  Files under `docs/` are plain files and are edited directly; files under
  `.spektacular/specs/` and `.spektacular/plans/` are edited only through the
  Spektacular CLI (see AGENTS.md).
- **Respect the workspace boundaries.** [ADR 0004](docs/adr/0004-no-split-preserve-seams.md)
  keeps this a monorepo but defines extraction seams between
  `tuna-shell-control` (framing/messages), `supervise.rs` (std-only), and
  shell-host (wire-only, no private compositor APIs). Preserve these
  boundaries even though the crates currently live together.

## Documentation and planning-only contributions

Given the current stage, doc fixes, ADR proposals, and spec/plan work are
welcome and don't require the full code-contribution checklist above — just
DCO sign-off on the commit. Use plain file edits for anything under `docs/`,
`README.md`, or this file; use the Spektacular CLI for anything under
`.spektacular/`.

## Test strategy

See [docs/test-strategy.md](docs/test-strategy.md) for the full verification
layers (unit/property, protocol probes, nested integration, VM/session,
hardware qualification, human UX/accessibility, security, performance,
upgrade/recovery) and the required cross-cutting journeys. Not all layers
have tooling yet — the framework is currently in draft status with no
implementation tests run in most layers beyond what CI already exercises.

## Reporting issues

Open a GitHub issue describing the gap, bug, or proposal. If it's substantial
enough to need multiple requirements worked out, expect to be routed toward
the spec workflow above rather than a direct code change.
