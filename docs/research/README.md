# Research intake and evidence rules

Use this checklist when creating a Spektacular plan's `research.md` or reviewing an upstream report.

## GNOME issue/work-item record

For each report relied on, capture:

- Project, exact issue/work-item ID, and direct URL.
- Current state, labels, milestone, assignee/owner, last activity date.
- Affected and fixed release versions; linked merge requests/commits and release notes.
- Reproduction steps, hardware/software configuration, and whether it reproduces on the pinned GNOME baseline.
- User impact and confidence; distinguish the reporter's claim from verified behavior.
- Tuna Desktop spek and test journey that covers it; state whether the opportunity is parity, a new improvement, or no longer applicable.

If direct GitLab access is unavailable, mark state and resolution **unverified**. Do not promote indexed search text into a claim that a bug remains open. Prefer official protocol/project documentation and upstream merge requests for implementation facts.

## Technical research record

Every plan research document should record question, source/date, observed evidence, assumptions, competing options, impact on the plan, and a follow-up owner/date for unresolved points. Pin versions for implementation-sensitive facts. Separate what is known from what is proposed.

## Benchmark evidence record

Record the baseline image/release, host/kernel, hardware and firmware, GPU/driver, output modes/scales, power profile, services/extensions, app workload, cold/warm procedure, sample count, raw artifact location, summary statistics, variance/noise, and limitations. Preserve redacted raw traces and script revision. Do not compare across unlike machines and infer a universal result.
