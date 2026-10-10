"""Per-phase, per-process metrics and the GNOME-relative budget gate (#315, #503).

Every metric is measured on Tuna Desktop and on the GNOME 51 session of the
same pair (same job, image and VM profile), so a budget is a ratio to GNOME
plus an optional absolute ceiling. Budgets live in docs/perf/budgets.json and
only ratchet tighter; see docs/perf/README.md.
"""
import json
import math
from pathlib import Path
import re
import statistics

# Guest CLOCK_BOOTTIME markers written by scripts/tuna-vm-perf, in workload
# order. A phase is reported only when both of its markers exist.
PHASES = ("idle", "frames", "typing", "overview", "workspaces", "window_cycling",
          "notifications", "settle")
ROLES = ("compositor", "shell", "xwayland", "portals", "services", "apps")
# Desktop is what Tuna Desktop owns: compositor plus shell. On GNOME both
# live in gnome-shell, so the comparison is the same set of work.
AGGREGATES = {"desktop": ("compositor", "shell"), "total": ROLES}
MIN_PHASE_INTERVALS = 3
DESKTOPS = ("gnome", "tuna")

_COMPOSITOR = {"tuna-compositor", "gnome-shell", "mutter"}
_SHELL_PREFIXES = ("tuna-shell-", "gnome-shell-calendar-server", "gnome-shell-portal-helper")
_SERVICE_PREFIXES = ("tuna-session", "tuna-ibus-bridge", "gnome-session", "gsd-", "ibus", "dbus",
                     "pipewire", "wireplumber", "at-spi", "gvfs", "systemd", "evolution-", "goa-",
                     "gnome-keyring", "localsearch", "tracker", "dconf", "xdg-document-portal",
                     "xdg-permission-store", "gcr-", "colord", "geoclue", "obexd", "mpris")


def process_name(row):
    """argv[0] basename when recorded, else the kernel's 15-byte comm."""
    for key in ("cmd", "comm"):
        value = row.get(key)
        if isinstance(value, str) and value:
            return value
    return None


def classify(name):
    if name is None:
        return None
    if name in _COMPOSITOR:
        return "compositor"
    if name.startswith(_SHELL_PREFIXES) or name == "gjs":
        # GNOME Shell runs its notification and screencast services under gjs.
        return "shell"
    if name == "Xwayland":
        return "xwayland"
    if name.startswith("xdg-desktop-portal"):
        return "portals"
    if name.startswith(_SERVICE_PREFIXES):
        return "services"
    return "apps"


def distribution(values):
    values = sorted(values)
    if not values:
        return {"count": 0}
    def percentile(p):
        return values[min(len(values) - 1, max(0, math.ceil(p * len(values)) - 1))]
    return {"count": len(values), "p50": percentile(.50), "p95": percentile(.95),
            "p99": percentile(.99), "min": values[0], "max": values[-1],
            "mean": statistics.mean(values)}


def cpu_intervals(samples):
    """Per-interval CPU percent of one core by role, from per-process ticks.

    Only processes present in both samples count, like the observer's own
    total, so a process start is never charged its lifetime ticks at once.
    Returns (begin_boottime, end_boottime, {role: percent}, named).
    """
    result = []
    for before, after in zip(samples, samples[1:]):
        elapsed = after.get("monotonic_s", 0) - before.get("monotonic_s", 0)
        hz = after.get("hz")
        begin, end = after.get("cpu_interval_start_boottime_s"), after.get("boottime_s")
        if not elapsed > 0 or not hz or begin is None or end is None:
            continue
        previous = {(p["pid"], p["start_ticks"]): p["cpu_ticks"] for p in before["processes"]}
        roles = dict.fromkeys(ROLES, 0.0)
        named = True
        for process in after["processes"]:
            ticks = previous.get((process["pid"], process["start_ticks"]))
            if ticks is None:
                continue
            role = classify(process_name(process))
            if role is None:
                named = False
                role = "apps"
            roles[role] += 100 * max(0, process["cpu_ticks"] - ticks) / hz / elapsed
        for name, members in AGGREGATES.items():
            roles[name] = sum(roles[member] for member in members)
        result.append((begin, end, roles, named))
    return result


def phase_windows(guest_phases):
    windows = {}
    for phase in PHASES:
        start, end = guest_phases.get(f"{phase}_start"), guest_phases.get(f"{phase}_end")
        if start is not None and end is not None and 0 < start < end:
            windows[phase] = (start, end)
    return windows


def phase_cpu(samples, guest_phases):
    """{phase: {role: distribution}} over intervals wholly inside the phase.

    Roles are omitted (only "total" kept) when the samples carry no process
    names, as in observers older than this module.
    """
    intervals = cpu_intervals(samples)
    result = {}
    for phase, (start, end) in phase_windows(guest_phases).items():
        inside = [row for row in intervals if start <= row[0] <= row[1] <= end]
        if len(inside) < MIN_PHASE_INTERVALS:
            continue
        named = all(row[3] for row in inside)
        keys = ROLES + tuple(AGGREGATES) if named else ("total",)
        result[phase] = {key: distribution([row[2][key] for row in inside]) for key in keys}
        result[phase]["named"] = named
        result[phase]["window_boottime_s"] = [start, end]
    return result


def read_session(directory):
    directory = Path(directory)
    report = json.loads((directory / "report.json").read_text())
    samples = json.loads((directory / "samples.json").read_text())
    guest_phases = json.loads((directory / "guest-phases.json").read_text())
    observations = json.loads((directory / "observations.json").read_text())
    return report, samples, guest_phases, observations


def session_metrics(directory):
    """Flat {metric: value} for one measured desktop session."""
    report, samples, guest_phases, observations = read_session(directory)
    metrics = {}
    for phase, roles in phase_cpu(samples, guest_phases).items():
        for role, values in roles.items():
            if isinstance(values, dict) and values.get("count"):
                metrics[f"cpu.{phase}.{role}.mean"] = values["mean"]
                metrics[f"cpu.{phase}.{role}.p95"] = values["p95"]
    whole = [s["cpu_observed_percent"] for s in samples if s.get("cpu_observed_percent") is not None]
    if whole:
        metrics["cpu.whole.total.p50"] = distribution(whole)["p50"]
    response = [row["upper_s"] * 1000 for row in observations if "upper_s" in row]
    if response:
        overview = distribution(response)
        metrics["overview.response_ms.p50"] = overview["p50"]
        metrics["overview.response_ms.p95"] = overview["p95"]
    pacing = (report.get("presentation_cadence") or {}).get("interval_ms") or {}
    for key in ("p95", "p99"):
        if key in pacing:
            metrics[f"frame_pacing.interval_ms.{key}"] = pacing[key]
    return metrics


def discover_pairs(root):
    """Completed GNOME/Tuna pairs under a performance-baseline artifact tree."""
    root = Path(root)
    pairs = []
    for profile, base in (("diagnostic", root), ("stock", root / "stock")):
        for repeat in (1, 2, 3):
            directory = base if repeat == 1 else base / f"repeat-{repeat}"
            sessions = {desktop: directory / desktop for desktop in DESKTOPS}
            if all((path / "report.json").is_file() for path in sessions.values()):
                pairs.append({"profile": profile, "repeat": repeat, **sessions})
    return pairs


def compare(pairs):
    """{metric: [{"profile", "repeat", "gnome", "tuna"}]} over all pairs."""
    result = {}
    for pair in pairs:
        gnome, tuna = session_metrics(pair["gnome"]), session_metrics(pair["tuna"])
        for metric in sorted(gnome.keys() & tuna.keys()):
            result.setdefault(metric, []).append({"profile": pair["profile"], "repeat": pair["repeat"],
                                                  "gnome": gnome[metric], "tuna": tuna[metric]})
    return result


def ratio(tuna, gnome, floor):
    return tuna / max(gnome, floor, 1e-9)


def evaluate(budgets, comparison, min_pairs=3):
    """Check every budget against the median over pairs of the Tuna/GNOME ratio.

    The median absorbs one disturbed pair; an enforced budget that is over its
    ratio or ceiling, or has fewer than min_pairs measurements, fails.
    """
    rows = []
    for metric, budget in sorted(budgets["metrics"].items()):
        floor = budget.get("floor", 0)
        values = comparison.get(metric, [])
        row = {"metric": metric, "enforced": bool(budget.get("enforced")), "pairs": len(values),
               "max_ratio": budget["max_ratio"], "ceiling": budget.get("ceiling"),
               "unit": budget.get("unit", "")}
        if len(values) < min_pairs:
            row.update(status="missing" if row["enforced"] else "warn",
                       reason=f"{len(values)} of {min_pairs} required pairs measured")
            rows.append(row)
            continue
        row["ratio"] = statistics.median(ratio(v["tuna"], v["gnome"], floor) for v in values)
        row["tuna"] = statistics.median(v["tuna"] for v in values)
        row["gnome"] = statistics.median(v["gnome"] for v in values)
        reasons = []
        if row["ratio"] > budget["max_ratio"]:
            reasons.append(f"ratio {row['ratio']:.2f} > budget {budget['max_ratio']:.2f}")
        if row["ceiling"] is not None and row["tuna"] > row["ceiling"]:
            reasons.append(f"Tuna {row['tuna']:.1f} > ceiling {row['ceiling']:.1f}")
        if reasons:
            row.update(status="fail" if row["enforced"] else "warn", reason="; ".join(reasons))
        else:
            row["status"] = "pass"
        row["meets_target"] = row["ratio"] <= budgets.get("target_ratio", 1.0)
        rows.append(row)
    return rows


def failed(rows):
    return [row for row in rows if row["status"] in ("fail", "missing")]


def propose(budgets, rows, margin=None, evidence=None):
    """Tighten each measured budget to ratio x (1 + margin), never loosening."""
    margin = budgets.get("margin", 0.15) if margin is None else margin
    proposed = json.loads(json.dumps(budgets))
    for row in rows:
        if "ratio" not in row:
            continue
        budget = proposed["metrics"][row["metric"]]
        candidate = math.ceil(row["ratio"] * (1 + margin) * 20) / 20
        candidate = max(candidate, budgets.get("target_ratio", 1.0))
        if candidate < budget["max_ratio"]:
            budget["max_ratio"] = candidate
            if evidence:
                budget["evidence"] = evidence
    return proposed


RUN_URL = re.compile(r"^https://github\.com/tuna-os/tuna-desktop/actions/runs/\d+$")


def ratchet_violations(base, head, repo_root=None):
    """Budgets may only tighten. Every change cites a run; loosening needs an ADR."""
    problems = []
    for metric, old in sorted(base["metrics"].items()):
        new = head["metrics"].get(metric)
        if new is None:
            problems.append(f"{metric}: removing a budget loosens the gate (needs an ADR, keep it with adr set)")
            continue
        changed = any(old.get(key) != new.get(key) for key in ("max_ratio", "ceiling", "floor", "enforced"))
        if not changed:
            continue
        loosened = []
        if new["max_ratio"] > old["max_ratio"]:
            loosened.append("max_ratio raised")
        if old.get("ceiling") is not None and (new.get("ceiling") is None or new["ceiling"] > old["ceiling"]):
            loosened.append("ceiling raised or removed")
        if new.get("floor", 0) > old.get("floor", 0):
            loosened.append("floor raised")
        if old.get("enforced") and not new.get("enforced"):
            loosened.append("enforcement dropped")
        if new.get("evidence") == old.get("evidence") or not RUN_URL.match(str(new.get("evidence", ""))):
            problems.append(f"{metric}: changed without new run evidence (set evidence to the "
                            "performance-baseline run URL that justifies it)")
        if loosened:
            adr = new.get("adr")
            exists = adr and repo_root is not None and (Path(repo_root) / adr).is_file()
            if not adr or adr == old.get("adr") or not str(adr).startswith("docs/adr/") or not exists:
                problems.append(f"{metric}: {', '.join(loosened)}; loosening needs a new ADR under "
                                "docs/adr/ referenced by adr (roadmap change control)")
    for metric, new in sorted(head["metrics"].items()):
        if metric not in base["metrics"] and not RUN_URL.match(str(new.get("evidence", ""))):
            problems.append(f"{metric}: a new budget needs run evidence")
    if head.get("target_ratio", 1.0) > base.get("target_ratio", 1.0):
        problems.append("target_ratio raised")
    return problems


def validate(budgets):
    problems = []
    if budgets.get("schema") != 1 or not isinstance(budgets.get("metrics"), dict):
        return ["budgets schema 1 with a metrics object required"]
    for metric, budget in budgets["metrics"].items():
        parts = metric.split(".")
        known = (len(parts) == 4 and parts[0] == "cpu" and (parts[1] in PHASES or parts[1] == "whole")
                 and parts[2] in ROLES + tuple(AGGREGATES) and parts[3] in ("mean", "p95", "p50")) or \
            metric in ("overview.response_ms.p50", "overview.response_ms.p95",
                       "frame_pacing.interval_ms.p95", "frame_pacing.interval_ms.p99")
        if not known:
            problems.append(f"{metric}: unknown metric")
        value = budget.get("max_ratio")
        if type(value) not in (int, float) or not value > 0:
            problems.append(f"{metric}: max_ratio must be positive")
        ceiling = budget.get("ceiling")
        if ceiling is not None and (type(ceiling) not in (int, float) or not ceiling > 0):
            problems.append(f"{metric}: ceiling must be positive or null")
        if type(budget.get("enforced")) is not bool:
            problems.append(f"{metric}: enforced must be true or false")
    return problems


def attribution(comparison):
    """{desktop: {phase: {role: median over pairs of the phase mean}}}."""
    result = {desktop: {} for desktop in DESKTOPS}
    for phase in PHASES:
        for role in ROLES:
            values = comparison.get(f"cpu.{phase}.{role}.mean")
            if not values:
                continue
            for desktop in DESKTOPS:
                result[desktop].setdefault(phase, {})[role] = statistics.median(v[desktop] for v in values)
    return result


def markdown(rows, pairs, budgets, comparison=None):
    lines = ["## Performance budgets: Tuna Desktop vs GNOME 51", "",
             f"{len(pairs)} paired sessions. Ratio is the median over pairs of Tuna / GNOME "
             f"(target <= {budgets.get('target_ratio', 1.0):.2f}). Budgets: `docs/perf/budgets.json`.", "",
             "| Metric | Status | GNOME | Tuna | Ratio | Budget | Ceiling |",
             "|---|---|---:|---:|---:|---:|---:|"]
    for row in rows:
        def number(key, fmt):
            return format(row[key], fmt) if row.get(key) is not None else "-"
        status = row["status"] + ("" if row["status"] == "pass" else f" ({row.get('reason', '')})")
        lines.append(f"| `{row['metric']}` | {status} | {number('gnome', '.1f')} | {number('tuna', '.1f')} | "
                     f"{number('ratio', '.2f')} | {row['max_ratio']:.2f} | {number('ceiling', '.1f')} |")
    for desktop, phases in attribution(comparison or {}).items():
        if not phases:
            continue
        lines += ["", f"### {'GNOME 51' if desktop == 'gnome' else 'Tuna Desktop'}: mean CPU by process role "
                  "(% of one core, median over pairs)", "",
                  "| Phase | " + " | ".join(ROLES) + " |", "|---|" + "---:|" * len(ROLES)]
        for phase, roles in phases.items():
            lines.append(f"| {phase} | " + " | ".join(format(roles.get(role, 0), ".1f") for role in ROLES) + " |")
    return "\n".join(lines) + "\n"
