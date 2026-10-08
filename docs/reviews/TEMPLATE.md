# Visual review: vX.Y.Z

Copy to `docs/reviews/vX.Y.Z.md` before tagging. `scripts/roost-release`
refuses a release build for a tag without this file, and refuses one that
still has unfilled placeholders (`TBD`).

## Reviewer

- Name: TBD
- Date: TBD

## Inputs

- Tuna Desktop CI run id (proof frames): TBD
- Baseline bundle (GNOME 51, `ghcr.io/tuna-os/marlin:gnome`, image digest): TBD

## Journeys

One row per journey stage. Verdict is `match`, `deviation` (link the
parity-ledger row), or `regression` (blocks the release).

| Journey stage | Tuna Desktop frame | Baseline frame | Verdict | Notes |
|---|---|---|---|---|
| Overview open | TBD | TBD | TBD | |
| Search typed | TBD | TBD | TBD | |
| App launched from search | TBD | TBD | TBD | |
| Workspace switch | TBD | TBD | TBD | |
| Alt-Tab switcher | TBD | TBD | TBD | |
| Tiled window | TBD | TBD | TBD | |
| Quick settings open | TBD | TBD | TBD | |
| Notification banner | TBD | TBD | TBD | |
| Lock screen | TBD | TBD | TBD | |

## Discrepancies filed

- Ledger rows added or changed: TBD
- Issues filed: TBD

## Sign-off

- [ ] No `regression` verdicts remain
- [ ] Every `deviation` has a ledger row with an owner
