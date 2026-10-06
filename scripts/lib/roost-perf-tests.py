#!/usr/bin/env python3
"""Resource accounting, process identity, trace parsing and bounded statistics."""
import importlib.machinery
import importlib.util
import json
import base64
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
    def globals_serial(self, raw, records=None):
        records = guest.global_records(raw, "wayland-0") if records is None else records
        return "\n".join("journal prefix roost-perf-globals: " + json.dumps(row)
                         for row in records)

    def test_globals_preserve_multichunk_raw_bytes(self):
        raw = (b"interface: 'wl_compositor', version: 6\n"
               b"interface: 'xdg_wm_base', version: 6\n" + b"a" * 7000)
        text, meta = host.raw_globals(self.globals_serial(raw))
        self.assertEqual(text.encode(), raw)
        self.assertEqual(meta["socket"], "wayland-0")

    def test_globals_partial_duplicate_and_corrupt_captures_fail(self):
        raw = (b"interface: 'wl_compositor'\ninterface: 'xdg_wm_base'\n" + b"a" * 7000)
        rows = guest.global_records(raw, "wayland-0")
        for broken in (rows[:-1], rows + [rows[0]],
                       [dict(row, sha256="0" * 64) for row in rows]):
            with self.subTest(broken=broken[0]["sha256"]), self.assertRaises(ValueError):
                host.raw_globals(self.globals_serial(raw, broken))

    def test_globals_missing_and_private_socket_are_not_desktop_evidence(self):
        with self.assertRaises(ValueError):
            host.raw_globals("")
        with self.assertRaises(ValueError):
            host.raw_globals(self.globals_serial(b"interface: 'zwp_input_method_manager_v2'"))

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

    def test_drm_duplicate_handles_and_shared_process_clients_are_counted_once(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for pid in (1, 2):
                path = self.process(root, pid, 0, 1000, 10, 55)
                (path / "fdinfo").mkdir()
                for fd in (0, 1):
                    (path / f"fdinfo/{fd}").write_text(
                        "drm-driver: test\ndrm-pdev: 0000:00:02.0\ndrm-client-id: 7\n"
                        "drm-resident-vram: 2 MiB\ndrm-shared-vram: 4 KiB\n"
                        "drm-total-memory: 128\nprivate-text: must not be retained\n")
            result = guest.sample(1000, root)
            self.assertEqual(result["drm_memory"]["status"], "available")
            self.assertEqual(len(result["drm_memory"]["clients"]), 1)
            client = result["drm_memory"]["clients"][0]
            self.assertEqual(client["memory_bytes"], {"drm-resident-vram": 2097152,
                                                     "drm-shared-vram": 4096,
                                                     "drm-total-memory": 128})
            self.assertNotIn("private-text", json.dumps(result))
            summary = host.drm_memory_summary([result])
            self.assertEqual(summary["sample_status_counts"], {"available": 1})
            self.assertEqual(next(c for c in summary["client_accounted_bytes"]
                                  if c["counter"] == "drm-resident-vram")["distribution"]["p50"], 2097152)

    def test_drm_unsupported_and_invalid_counters_are_not_measured_zero(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.process(root, 1, 0, 1000, 10, 55)
            absent = guest.sample(1000, root)
            self.assertEqual(absent["drm_memory"]["status"], "unavailable")
            (path / "fdinfo").mkdir()
            (path / "fdinfo/0").write_text(
                "drm-driver: test\ndrm-client-id: 1\ndrm-memory-vram: -2 KiB\n")
            invalid = guest.sample(1000, root)
            self.assertEqual(invalid["drm_memory"]["status"], "incomplete")
            summary = host.drm_memory_summary([absent, invalid])
            self.assertEqual(summary["sample_status_counts"], {"unavailable": 1, "incomplete": 1})
            self.assertEqual(summary["client_accounted_bytes"], [])

    def test_drm_measured_zero_and_separate_devices_preserve_their_meaning(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = self.process(root, 1, 0, 1000, 10, 55)
            (path / "fdinfo").mkdir()
            for fd in (0, 1):
                (path / f"fdinfo/{fd}").write_text(
                    f"drm-driver: test\ndrm-pdev: 0000:00:0{fd}.0\ndrm-client-id: 1\n"
                    "drm-memory-vram: 0 KiB\n")
            result = guest.sample(1000, root)
            self.assertEqual(result["drm_memory"]["status"], "available")
            self.assertEqual(len(result["drm_memory"]["clients"]), 2)
            summary = host.drm_memory_summary([result])
            self.assertEqual(len(summary["client_accounted_bytes"]), 2)
            self.assertTrue(all(c["distribution"]["p50"] == 0
                                for c in summary["client_accounted_bytes"]))

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

    def test_idle_cpu_excludes_intervals_crossing_either_guest_clock_boundary(self):
        rows = [{"cpu_interval_start_boottime_s": i, "boottime_s": i+1,
                 "cpu_observed_percent": 2} for i in range(100, 120)]
        rows += [{"cpu_interval_start_boottime_s": 99, "boottime_s": 101,
                  "cpu_observed_percent": 999},
                 {"cpu_interval_start_boottime_s": 119, "boottime_s": 121,
                  "cpu_observed_percent": 999},
                 {"cpu_interval_start_boottime_s": None, "boottime_s": 100,
                  "cpu_observed_percent": None}]
        result = host.idle_cpu(rows, 100, 120)
        self.assertEqual(result["count"], 20)
        self.assertEqual(result["max"], 2)
        with self.assertRaisesRegex(ValueError, "fewer than 20"):
            host.idle_cpu(rows, 101, 120)
        with self.assertRaisesRegex(ValueError, "invalid guest idle"):
            host.idle_cpu(rows, 120, 100)

    def test_guest_phase_probe_requires_executed_complete_valid_output(self):
        class Agent:
            def __init__(self, result, **flags):
                self.commands = []
                self.status = {"exited": True, "exitcode": 0,
                               "out-data": base64.b64encode(json.dumps(result).encode()).decode(),
                               **flags}

            def command(self, name, **arguments):
                self.commands.append((name, arguments))
                return {"pid": 7} if name == "guest-exec" else self.status

        agent = Agent({"boottime_s": 100, "notification_id": 9})
        self.assertEqual(host.guest_probe(agent, "notify", 3)["notification_id"], 9)
        self.assertEqual(agent.commands[0][1]["path"], "/usr/libexec/roost-perf-phase")
        self.assertEqual(agent.commands[0][1]["arg"], ["notify", "--index", "3"])
        for flags in ({"exitcode": 1}, {"out-truncated": True}, {"err-truncated": True}):
            with self.subTest(flags=flags), self.assertRaisesRegex(RuntimeError, "failed"):
                host.guest_probe(Agent({"boottime_s": 100}, **flags), "clock")
        for clock in (None, True, -1, float("nan")):
            with self.subTest(clock=clock), self.assertRaisesRegex(ValueError, "invalid guest clock"):
                host.guest_probe(Agent({"boottime_s": clock}), "clock")
        for identity in (0, True, None, 0x100000000):
            with self.subTest(identity=identity), self.assertRaisesRegex(ValueError, "notification ID"):
                host.guest_probe(Agent({"boottime_s": 100, "notification_id": identity}), "notify", 0)
        for action, index in (("exec", None), ("notify", -1), ("notify", 10), ("notify", True)):
            with self.assertRaises(ValueError):
                host.guest_probe(agent, action, index)


if __name__ == "__main__":
    unittest.main()
