#!/usr/bin/env python3
"""Unit tests for the GNOME-relative performance budget gate (#315).

Synthetic artifact trees only; the real paired measurement stays in
performance-baseline.yml.
"""
import contextlib
import copy
import io
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/lib"))
import perf_budget as pb  # noqa: E402

loader = importlib.machinery.SourceFileLoader("gate", str(ROOT / "scripts/tuna-perf-gate"))
spec = importlib.util.spec_from_loader("gate", loader)
gate = importlib.util.module_from_spec(spec)
loader.exec_module(gate)

RUN = "https://github.com/tuna-os/tuna-desktop/actions/runs/{}"
PHASES = {"idle": (10, 40), "overview": (50, 70), "settle": (80, 100)}


def samples(load, names=True, start=0, end=101):
    """One sample per second; load maps (phase or None) to {name: percent}."""
    rows, ticks = [], {}
    for second in range(start, end):
        phase = next((p for p, (a, b) in PHASES.items() if a <= second - 1 and second <= b), None)
        per = load.get(phase, load.get(None, {}))
        processes = []
        for name, percent in sorted(per.items()):
            ticks[name] = ticks.get(name, 0) + percent  # hz=100, one second
            pid = 100 + list(ticks).index(name)
            row = {"pid": pid, "start_ticks": pid, "cpu_ticks": round(ticks[name])}
            if names:
                row.update(comm=name[:15], cmd=name)
            processes.append(row)
        rows.append({"monotonic_s": float(second), "boottime_s": float(second), "hz": 100,
                     "cpu_interval_start_boottime_s": float(second - 1) if second > start else None,
                     "cpu_observed_percent": sum(per.values()) if second > start else None,
                     "processes": processes})
    return rows


def write_session(directory, load, overview_ms=200, pacing=16.7, names=True):
    directory.mkdir(parents=True)
    phases = {f"{p}_{edge}": float(v) for p, (a, b) in PHASES.items() for edge, v in (("start", a), ("end", b))}
    (directory / "report.json").write_text(json.dumps(
        {"presentation_cadence": {"interval_ms": {"p95": pacing, "p99": pacing * 2}}}))
    (directory / "samples.json").write_text(json.dumps(samples(load, names)))
    (directory / "guest-phases.json").write_text(json.dumps(phases))
    (directory / "observations.json").write_text(json.dumps(
        [{"index": i, "upper_s": overview_ms / 1000} for i in range(20)]))


GNOME = {None: {"gnome-shell": 2}, "overview": {"gnome-shell": 20, "nautilus": 5}}
TUNA = {None: {"tuna-compositor": 2, "tuna-shell-gtk": 1},
        "overview": {"tuna-compositor": 30, "tuna-shell-gtk": 6, "Xwayland": 2, "nautilus": 5}}


def tree(root, tuna=TUNA, outlier=None, pairs=3, **kwargs):
    for index in range(pairs):
        base = Path(root) / ("" if index == 0 else f"repeat-{index + 1}")
        write_session(base / "gnome", GNOME)
        write_session(base / "tuna", outlier if (outlier and index == 2) else tuna, **kwargs)


def budgets(**metrics):
    return {"schema": 1, "target_ratio": 1.0, "margin": 0.15, "metrics": metrics}


def budget(max_ratio, ceiling=None, floor=1.0, enforced=True, evidence=RUN.format(1), **extra):
    return dict(max_ratio=max_ratio, ceiling=ceiling, floor=floor, enforced=enforced, evidence=evidence, **extra)


class Attribution(unittest.TestCase):
    def test_process_names_map_to_roles(self):
        for name, role in (("tuna-compositor", "compositor"), ("gnome-shell", "compositor"),
                           ("tuna-shell-gtk", "shell"), ("tuna-shell-host", "shell"), ("gjs", "shell"),
                           ("Xwayland", "xwayland"), ("xdg-desktop-portal-gnome", "portals"),
                           ("xdg-desktop-portal", "portals"), ("gsd-color", "services"),
                           ("tuna-session", "services"), ("pipewire", "services"),
                           ("nautilus", "apps"), ("tuna-frame-pacer", "apps"), (None, None)):
            self.assertEqual(pb.classify(name), role, name)

    def test_argv_name_wins_over_truncated_comm(self):
        self.assertEqual(pb.process_name({"comm": "xdg-desktop-por", "cmd": "xdg-desktop-portal-gtk"}),
                         "xdg-desktop-portal-gtk")
        self.assertEqual(pb.process_name({"comm": "Xwayland", "cmd": None}), "Xwayland")
        self.assertIsNone(pb.process_name({}))

    def test_intervals_attribute_roles_and_never_charge_a_new_process_its_history(self):
        rows = samples({None: {"tuna-compositor": 10, "tuna-shell-gtk": 4}}, start=0, end=3)
        rows[2]["processes"].append({"pid": 900, "start_ticks": 900, "cpu_ticks": 5000, "cmd": "nautilus"})
        intervals = pb.cpu_intervals(rows)
        self.assertEqual(len(intervals), 2)
        _, _, roles, named = intervals[1]
        self.assertTrue(named)
        self.assertAlmostEqual(roles["compositor"], 10)
        self.assertAlmostEqual(roles["shell"], 4)
        self.assertAlmostEqual(roles["desktop"], 14)
        self.assertAlmostEqual(roles["apps"], 0)
        self.assertAlmostEqual(roles["total"], 14)

    def test_phases_use_only_wholly_contained_intervals_and_need_three(self):
        rows = samples({None: {"tuna-compositor": 1}, "overview": {"tuna-compositor": 50}})
        result = pb.phase_cpu(rows, {"overview_start": 50.0, "overview_end": 70.0,
                                     "idle_start": 10.0, "idle_end": 12.5, "typing_start": 5.0})
        self.assertEqual(set(result), {"overview"})  # idle has two intervals; typing has no end
        self.assertEqual(result["overview"]["compositor"]["count"], 20)
        self.assertAlmostEqual(result["overview"]["compositor"]["mean"], 50)
        self.assertTrue(result["overview"]["named"])

    def test_unnamed_samples_report_only_the_total(self):
        result = pb.phase_cpu(samples({None: {"a": 3}}, names=False), {"idle_start": 10.0, "idle_end": 40.0})
        self.assertEqual(set(result["idle"]) - {"named", "window_boottime_s"}, {"total"})
        self.assertFalse(result["idle"]["named"])


def run_gate(args):
    with contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
        return gate.main(args)


class Gate(unittest.TestCase):
    def evaluate(self, budget_file, **kwargs):
        with tempfile.TemporaryDirectory() as directory:
            tree(directory, **kwargs)
            pairs = pb.discover_pairs(directory)
            self.assertEqual(len(pairs), kwargs.get("pairs", 3))
            return {row["metric"]: row for row in pb.evaluate(budget_file, pb.compare(pairs))}

    def test_pairs_are_discovered_for_both_profiles(self):
        with tempfile.TemporaryDirectory() as directory:
            tree(directory)
            tree(Path(directory) / "stock", pairs=2)
            pairs = pb.discover_pairs(directory)
            self.assertEqual([(p["profile"], p["repeat"]) for p in pairs],
                             [("diagnostic", 1), ("diagnostic", 2), ("diagnostic", 3), ("stock", 1), ("stock", 2)])

    def test_ratio_and_ceiling_pass_and_fail(self):
        rows = self.evaluate(budgets(**{
            "cpu.overview.desktop.mean": budget(1.9),            # 36 / 20 = 1.8
            "cpu.overview.total.mean": budget(1.5),              # 43 / 25 = 1.72
            "cpu.overview.compositor.mean": budget(2.0, ceiling=25),
            "overview.response_ms.p50": budget(1.0, floor=0)}), overview_ms=200)
        self.assertEqual(rows["cpu.overview.desktop.mean"]["status"], "pass")
        self.assertAlmostEqual(rows["cpu.overview.desktop.mean"]["ratio"], 1.8)
        self.assertEqual(rows["cpu.overview.total.mean"]["status"], "fail")
        self.assertIn("ratio 1.72 > budget 1.50", rows["cpu.overview.total.mean"]["reason"])
        self.assertEqual(rows["cpu.overview.compositor.mean"]["status"], "fail")
        self.assertIn("ceiling", rows["cpu.overview.compositor.mean"]["reason"])
        self.assertTrue(rows["overview.response_ms.p50"]["meets_target"])

    def test_floor_keeps_a_near_zero_gnome_value_from_exploding_the_ratio(self):
        quiet = copy.deepcopy(TUNA)
        quiet[None] = {"tuna-compositor": 0.5}
        gnome_idle = {"cpu.idle.total.mean": budget(1.0, floor=1.0)}
        rows = self.evaluate(budgets(**gnome_idle), tuna=quiet)
        # GNOME idles at 2% here; with a 1% floor 0.5% is comfortably within.
        self.assertEqual(rows["cpu.idle.total.mean"]["status"], "pass")
        self.assertAlmostEqual(pb.ratio(0.5, 0.0, 1.0), 0.5)

    def test_median_absorbs_one_disturbed_pair(self):
        noisy = copy.deepcopy(TUNA)
        noisy["overview"] = {"tuna-compositor": 90}
        rows = self.evaluate(budgets(**{"cpu.overview.desktop.mean": budget(1.9)}), outlier=noisy)
        self.assertEqual(rows["cpu.overview.desktop.mean"]["status"], "pass")

    def test_provisional_budgets_warn_and_missing_enforced_metrics_fail(self):
        rows = self.evaluate(budgets(**{
            "cpu.overview.total.mean": budget(1.0, enforced=False),
            "cpu.window_cycling.total.mean": budget(2.0)}))
        self.assertEqual(rows["cpu.overview.total.mean"]["status"], "warn")
        self.assertEqual(rows["cpu.window_cycling.total.mean"]["status"], "missing")
        self.assertEqual([r["metric"] for r in pb.failed(list(rows.values()))], ["cpu.window_cycling.total.mean"])

    def test_too_few_pairs_fail_an_enforced_budget(self):
        rows = self.evaluate(budgets(**{"cpu.overview.total.mean": budget(5.0)}), pairs=2)
        self.assertEqual(rows["cpu.overview.total.mean"]["status"], "missing")

    def test_cli_exit_status_summary_and_report_only(self):
        with tempfile.TemporaryDirectory() as directory:
            tree(Path(directory) / "artifacts")
            path = Path(directory) / "budgets.json"
            path.write_text(json.dumps(budgets(**{"cpu.overview.total.mean": budget(1.5)})))
            summary, out = Path(directory) / "summary.md", Path(directory) / "gate.json"
            args = ["check", "--artifacts", str(Path(directory) / "artifacts"), "--budgets", str(path),
                    "--markdown", str(summary), "--out", str(out)]
            self.assertEqual(run_gate(args), 1)
            self.assertIn("`cpu.overview.total.mean` | fail", summary.read_text())
            self.assertIn("| overview | 30.0 | 6.0 | 1.9 | 0.0 | 0.0 | 4.8 |", summary.read_text())
            # Xwayland and the app start inside the phase: their first interval is not charged.
            self.assertEqual(json.loads(out.read_text())["results"][0]["status"], "fail")
            self.assertEqual(run_gate(args + ["--report-only"]), 0)
            path.write_text(json.dumps(budgets(**{"cpu.overview.total.mean": budget(2.0)})))
            self.assertEqual(run_gate(args), 0)

    def test_propose_only_tightens_never_below_target_and_records_evidence(self):
        current = budgets(**{"cpu.overview.total.mean": budget(3.0),
                             "cpu.overview.desktop.mean": budget(1.2),
                             "overview.response_ms.p50": budget(4.0, floor=0)})
        rows = self.evaluate(current, overview_ms=100)
        result = pb.propose(current, list(rows.values()), evidence=RUN.format(7))["metrics"]
        self.assertEqual(result["cpu.overview.total.mean"]["max_ratio"], 2.0)   # 1.72 x 1.15 -> 1.98 -> 2.00
        self.assertEqual(result["cpu.overview.total.mean"]["evidence"], RUN.format(7))
        self.assertEqual(result["cpu.overview.desktop.mean"]["max_ratio"], 1.2)  # never loosened
        self.assertEqual(result["cpu.overview.desktop.mean"]["evidence"], RUN.format(1))
        self.assertEqual(result["overview.response_ms.p50"]["max_ratio"], 1.0)   # 0.5 is past the target


class Ratchet(unittest.TestCase):
    base = budgets(**{"cpu.overview.total.mean": budget(2.0, ceiling=80)})

    def check(self, change, root=ROOT):
        head = copy.deepcopy(self.base)
        change(head["metrics"])
        return pb.ratchet_violations(self.base, head, root)

    def test_unchanged_and_evidenced_tightening_pass(self):
        self.assertEqual(self.check(lambda m: None), [])
        def tighten(m):
            m["cpu.overview.total.mean"].update(max_ratio=1.8, ceiling=70, evidence=RUN.format(2))
        self.assertEqual(self.check(tighten), [])

    def test_change_without_new_run_evidence_is_refused(self):
        problems = self.check(lambda m: m["cpu.overview.total.mean"].update(max_ratio=1.8))
        self.assertTrue(any("without new run evidence" in p for p in problems))
        problems = self.check(lambda m: m["cpu.overview.total.mean"].update(max_ratio=1.8, evidence="trust me"))
        self.assertTrue(any("without new run evidence" in p for p in problems))

    def test_loosening_needs_a_new_existing_adr(self):
        for change in ({"max_ratio": 2.5}, {"ceiling": 90}, {"ceiling": None}, {"floor": 5.0}, {"enforced": False}):
            problems = self.check(lambda m: m["cpu.overview.total.mean"].update(evidence=RUN.format(3), **change))
            self.assertTrue(any("needs a new ADR" in p for p in problems), change)
        with tempfile.TemporaryDirectory() as directory:
            (Path(directory) / "docs/adr").mkdir(parents=True)
            (Path(directory) / "docs/adr/0099-raise.md").write_text("ADR")
            ok = lambda m: m["cpu.overview.total.mean"].update(max_ratio=2.5, evidence=RUN.format(3),
                                                               adr="docs/adr/0099-raise.md")
            self.assertEqual(self.check(ok, directory), [])
            missing = lambda m: m["cpu.overview.total.mean"].update(max_ratio=2.5, evidence=RUN.format(3),
                                                                    adr="docs/adr/0100-absent.md")
            self.assertTrue(self.check(missing, directory))

    def test_removing_a_budget_and_unevidenced_additions_are_refused(self):
        self.assertTrue(any("removing" in p for p in self.check(lambda m: m.clear())))
        added = lambda m: m.update({"cpu.idle.total.mean": budget(1.5, evidence="")})
        self.assertTrue(any("new budget" in p for p in self.check(added)))
        added = lambda m: m.update({"cpu.idle.total.mean": budget(1.5)})
        self.assertEqual(self.check(added), [])


class CommittedBudgets(unittest.TestCase):
    def test_committed_file_is_valid_and_every_enforced_budget_is_evidenced(self):
        committed = json.loads((ROOT / "docs/perf/budgets.json").read_text())
        self.assertEqual(pb.validate(committed), [])
        self.assertEqual(pb.ratchet_violations(committed, committed, ROOT), [])
        for metric, entry in committed["metrics"].items():
            self.assertRegex(entry["evidence"], pb.RUN_URL, metric)
            self.assertGreaterEqual(entry["max_ratio"], committed["target_ratio"], metric)

    def test_validate_rejects_unknown_metrics_and_bad_values(self):
        problems = pb.validate(budgets(**{"cpu.nap.total.mean": budget(1.0),
                                          "cpu.idle.total.mean": budget(0, enforced="yes")}))
        self.assertEqual(len(problems), 3)


if __name__ == "__main__":
    unittest.main()
