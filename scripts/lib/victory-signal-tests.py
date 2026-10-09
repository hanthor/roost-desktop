#!/usr/bin/env python3
"""Self-test for scripts/victory-signal.py: ledger parsing, gap search, end-to-end signal."""
import importlib.machinery
import importlib.util
import json
import os
import stat
import tempfile
from contextlib import contextmanager
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "victory-signal.py"
loader = importlib.machinery.SourceFileLoader("victory_signal", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
V = importlib.util.module_from_spec(spec)
loader.exec_module(V)

LEDGER_FIXTURE = """\
# Parity ledger
| Area | Check | Status |
| --- | --- | --- |
| shell | overview | pass |
| shell | `search` | pass |
| input | ime | partial |
| capture | screencast | missing |
| perf | p95 | deviation |
| a11y | contrast | untested |
not a table row
| short | row |
"""


@contextmanager
def temp_script(body):
    with tempfile.TemporaryDirectory() as bindir:
        gh = Path(bindir) / "gh"
        gh.write_text(body)
        gh.chmod(gh.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        old_path = os.environ.get("PATH", "")
        os.environ["PATH"] = bindir + os.pathsep + old_path
        try:
            yield bindir
        finally:
            os.environ["PATH"] = old_path


def test_ledger_counts_parses_statuses_and_ignores_noise():
    with tempfile.TemporaryDirectory() as tmp:
        ledger = Path(tmp) / "ledger.md"
        ledger.write_text(LEDGER_FIXTURE)
        counts, note = V.ledger_counts(str(ledger))
    assert counts == {"pass": 2, "partial": 1, "missing": 1, "deviation": 1, "untested": 1}, counts
    assert note == "", note


def test_ledger_counts_empty_input_reports_note():
    with tempfile.TemporaryDirectory() as tmp:
        ledger = Path(tmp) / "ledger.md"
        ledger.write_text("# Parity ledger\n\n| Area | Check | Status |\n| --- | --- | --- |\n")
        counts, note = V.ledger_counts(str(ledger))
    assert sum(counts.values()) == 0, counts
    assert note == "no ledger rows parsed", note


def test_ledger_counts_missing_file_returns_note_not_crash():
    counts, note = V.ledger_counts("/nonexistent-dir/parity-ledger.md")
    assert sum(counts.values()) == 0, counts
    assert note, "expected a note explaining the missing ledger"


def test_open_gaps_parses_gh_count_and_survives_failure():
    with temp_script("#!/bin/sh\necho 3\n"):
        gaps, note = V.open_gaps()
    assert gaps == 3, gaps
    assert note == "", note
    with temp_script("#!/bin/sh\nexit 1\n"):
        gaps, note = V.open_gaps()
    assert gaps == -1, gaps
    assert note, "expected a note when gh fails"


def test_main_end_to_end_writes_signal():
    with tempfile.TemporaryDirectory() as root:
        docs = Path(root) / "docs"
        docs.mkdir()
        (docs / "parity-ledger.md").write_text(LEDGER_FIXTURE)
        with tempfile.TemporaryDirectory() as cwd, temp_script("#!/bin/sh\necho 2\n"):
            old_root = os.environ.get("VICTORY_ROOT")
            old_cwd = os.getcwd()
            os.environ["VICTORY_ROOT"] = root
            os.chdir(cwd)
            try:
                assert V.main() == 0
                signal = json.loads((Path(cwd) / "victory-signal.json").read_text())
            finally:
                os.chdir(old_cwd)
                if old_root is None:
                    os.environ.pop("VICTORY_ROOT", None)
                else:
                    os.environ["VICTORY_ROOT"] = old_root
    assert signal["ledger"]["rows"] == {
        "pass": 2, "partial": 1, "missing": 1, "deviation": 1, "untested": 1,
    }, signal["ledger"]
    assert signal["ledger"]["pass_fraction"] == 2 / 6, signal["ledger"]
    assert signal["gaps"]["open"] == 2, signal["gaps"]
    assert signal["perf"]["status"] == "pending", signal["perf"]
    assert signal["image"]["status"] == "pending", signal["image"]
    assert signal["generated"], "expected a generated timestamp"


def test_ledger_regression_visibly_breaks_signal():
    with tempfile.TemporaryDirectory() as root:
        docs = Path(root) / "docs"
        docs.mkdir()
        (docs / "parity-ledger.md").write_text(
            "| Area | Check | Status |\n| --- | --- | --- |\n| shell | overview | missing |\n"
        )
        with tempfile.TemporaryDirectory() as cwd, temp_script("#!/bin/sh\necho 5\n"):
            old_root = os.environ.get("VICTORY_ROOT")
            old_cwd = os.getcwd()
            os.environ["VICTORY_ROOT"] = root
            os.chdir(cwd)
            try:
                assert V.main() == 0
                signal = json.loads((Path(cwd) / "victory-signal.json").read_text())
            finally:
                os.chdir(old_cwd)
                if old_root is None:
                    os.environ.pop("VICTORY_ROOT", None)
                else:
                    os.environ["VICTORY_ROOT"] = old_root
    assert signal["ledger"]["pass_fraction"] == 0.0, signal["ledger"]
    assert signal["ledger"]["rows"]["missing"] == 1, signal["ledger"]
    assert signal["gaps"]["open"] == 5, signal["gaps"]


TESTS = [
    test_ledger_counts_parses_statuses_and_ignores_noise,
    test_ledger_counts_empty_input_reports_note,
    test_ledger_counts_missing_file_returns_note_not_crash,
    test_open_gaps_parses_gh_count_and_survives_failure,
    test_main_end_to_end_writes_signal,
    test_ledger_regression_visibly_breaks_signal,
]


if __name__ == "__main__":
    failed = 0
    for test in TESTS:
        try:
            test()
        except AssertionError as exc:
            failed += 1
            print(f"FAIL {test.__name__}: {exc}")
        else:
            print(f"ok {test.__name__}")
    print(f"{len(TESTS) - failed}/{len(TESTS)} passed")
    raise SystemExit(1 if failed else 0)
