#!/usr/bin/env python3
"""Publish the v1 victory signal from the trackers that own it.

Reads the parity ledger (pass fraction), the open gap-issue count,
and the verdicts of the benchmark-budget and image-qualification work
(pending until their lanes land), then writes victory-signal.json.
Informational by design: the signal carries red, the job stays green.
"""

import json
import os
import re
import subprocess
import sys
from datetime import datetime, timezone

REPO = os.environ.get("VICTORY_REPO", "tuna-os/tuna-desktop")
STATUSES = ("pass", "partial", "missing", "deviation", "untested")


def ledger_counts(path):
    counts = {s: 0 for s in STATUSES}
    try:
        with open(path, encoding="utf-8") as handle:
            lines = handle.readlines()
    except OSError as exc:
        return counts, f"ledger unreadable: {exc}"
    for line in lines:
        cells = [cell.strip().strip("`") for cell in line.strip().strip("|").split("|")]
        if len(cells) >= 3 and cells[2] in counts:
            counts[cells[2]] += 1
    if sum(counts.values()) == 0:
        return counts, "no ledger rows parsed"
    return counts, ""


def open_gaps():
    try:
        out = subprocess.run(
            [
                "gh",
                "issue",
                "list",
                "-R",
                REPO,
                "--search",
                '"[GNOME 51 gap]" state:open',
                "--json",
                "number",
                "--limit",
                "200",
                "--jq",
                "length",
            ],
            capture_output=True,
            text=True,
            timeout=120,
            check=True,
        )
        return int(out.stdout.strip()), ""
    except Exception as exc:  # noqa: BLE001 — signal must survive
        return -1, f"gap search unavailable: {exc}"


def main():
    root = os.environ.get("VICTORY_ROOT", ".")
    ledger, ledger_note = ledger_counts(os.path.join(root, "docs/parity-ledger.md"))
    total = sum(ledger.values())
    gaps, gaps_note = open_gaps()
    signal = {
        "generated": datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "ledger": {"rows": ledger, "pass_fraction": (ledger["pass"] / total) if total else 0.0},
        "gaps": {"open": gaps},
        "perf": {"status": "pending", "tracking": "#547"},
        "image": {"status": "pending", "tracking": "#273"},
        "notes": [note for note in (ledger_note, gaps_note) if note],
    }
    with open("victory-signal.json", "w", encoding="utf-8") as handle:
        json.dump(signal, handle, indent=2)
        handle.write("\n")
    summary = [
        "## Victory signal",
        "",
        f"- Ledger: {ledger['pass']}/{total} pass",
        f"- Open gaps: {gaps if gaps >= 0 else 'unknown'}",
        "- Perf verdict: pending (#547)",
        "- Image verdict: pending (#273)",
    ]
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        with open(summary_path, "a", encoding="utf-8") as handle:
            handle.write("\n".join(summary) + "\n")
    print("\n".join(summary))
    return 0


if __name__ == "__main__":
    sys.exit(main())
