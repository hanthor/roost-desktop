#!/usr/bin/env python3
"""Resource accounting, process identity, trace parsing and bounded statistics."""
import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


guest = load("guest", ROOT / "scripts/lib/roost-perf-guest.py")
host = load("host", ROOT / "scripts/roost-vm-perf")


class Accounting(unittest.TestCase):
    def process(self, root, pid, ppid, uid, pss, start):
        path = root / str(pid)
        path.mkdir()
        (path / "status").write_text(f"Uid: {uid} {uid} {uid} {uid}\n")
        fields = ["0"] * 50
        fields[0] = "S";fields[1] = str(ppid)
        fields[11] = "30";fields[12] = "20";fields[19] = str(start)
        (path / "stat").write_text(f"{pid} (command ) with spaces) " + " ".join(fields))
        (path / "smaps_rollup").write_text(f"Pss: {pss} kB\nRss: {pss*2} kB\n")
        (path / "fd").mkdir()
        (path / "fd/0").touch()
        return path

    def test_scope_excludes_other_users_observer_and_descendants(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.process(root, 1, 0, 1000, 10, 55)
            self.process(root, 2, 1, 1000, 20, 66)
            self.process(root, 3, 2, 1000, 30, 77)
            self.process(root, 4, 0, 1000, 40, 88)
            self.process(root, 5, 0, 1001, 50, 99)
            result = guest.sample(1000, root, observer=1)
            self.assertEqual(result["pss_kib"], 40)
            self.assertEqual(result["process_count"], 1)
            self.assertEqual(result["fds"], 1)
            self.assertEqual(result["processes"][0]["cpu_ticks"], 50)
            self.assertEqual(result["processes"][0]["start_ticks"], 88)

    def test_memory_read_failure_is_reported_instead_of_understating_pss(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.process(root, 1, 0, 1000, 10, 55)
            (path / "smaps_rollup").unlink()
            result = guest.sample(1000, root)
            self.assertEqual(result["unreadable"], [{"pid":1,"error":"FileNotFoundError"}])

    def test_zombie_is_departed_even_before_parent_reaps_it(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.process(root, 1, 0, 1000, 10, 55)
            stat = (path / "stat").read_text().replace(") S ", ") Z ")
            (path / "stat").write_text(stat)
            (path / "smaps_rollup").unlink()
            result = guest.sample(1000, root)
            self.assertEqual(result["unreadable"], [])
            self.assertEqual(result["process_count"], 0)

    def test_journal_prefix_and_truncated_serial_do_not_corrupt_valid_samples(self):
        result = host.raw_samples('prefix roost-perf-sample: {"pss_kib":7}\nroost-perf-sample: {bad\n')
        self.assertEqual(result, [{"pss_kib":7}])

    def test_small_sample_percentiles_keep_the_extreme_tail(self):
        result = host.distribution([1, 9, 2, 3])
        self.assertEqual(result["count"], 4)
        self.assertEqual(result["p95"], 9)
        self.assertEqual(result["p99"], 9)
        self.assertEqual(host.distribution([]), {"count":0})


if __name__ == "__main__":
    unittest.main()
