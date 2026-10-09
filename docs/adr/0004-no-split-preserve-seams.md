# ADR 0004: No repo split; preserve extraction seams

- Status: Decided
- Date: 2026-09-28
- Owner: project (unassigned)
- Dependent speks: all (workspace layout constraint)

## Context

Raised 2026-09-28: with four crates incoming across waves 1–3
(`tuna-compositor`, `tuna-shell-control`, `tuna-shell-host`, plus the
planned supervisor/overlay growth), should any part spin off into its
own project? The Spektacular registry holds a single member repo, so a
split would also mean new repo registration, CI, and versioning.

## Decision

Stay a monorepo. Nothing today has an external consumer, an independent
release cadence, or a separate trust boundary that would justify
split-off overhead. Instead, preserve three extraction seams so a
future split stays a file move, not a rewrite:

1. **Framing vs messages** in `tuna-shell-control`: length-prefix,
   negotiation, and caps must not import Tuna-specific message types.
   (Today the crate mixes both; separate them before any split.)
2. **Supervisor stays std-only**: `supervise.rs` takes no compositor
   types and gains none. It remains portable by construction.
3. **Shell-host talks wire-only**: all compositor contact goes through
   the schema crate, never private APIs or shared memory layouts.

## Alternatives considered

- **Split the schema crate now**: rejected — zero consumers, and the
  envelope/message separation it needs as a precondition is itself
  undone work.
- **Split the supervisor now**: rejected — ~150 lines of policy the
  ecosystem (systemd, existing crates) covers better; we would be
  maintaining a worse systemd for no user.
- **Split compositor policy or shell-host**: rejected — Tuna-specific
  by definition, never split candidates.

## Consequences

- New crates land in this workspace by default; a split needs its own
  ADR with a named external consumer.
- Revisit triggers: a second project imports the envelope, the
  supervisor outgrows policy-into-service, or release cadences diverge.
- GPL-3.0-or-later applies workspace-wide, so licensing never forces a
  split either way.

## Evidence

- Workspace layout at decision time: `cargo metadata --no-deps`
  (`tuna-compositor`, `tuna-shell-control`, `tuna-shell-host`).
- Single-member registry: `spektacular repo list`.
