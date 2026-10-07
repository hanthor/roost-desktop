#!/usr/bin/python3
"""Fixture admission/shipping policy tests. These are not actual GPU evidence."""
import importlib.machinery
import importlib.util
import hashlib
import json
import io
import os
from pathlib import Path
import subprocess
import tempfile
import tomllib
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("night_light_fault_control", str(ROOT / "packaging/marlin/vm-lane/roost-vm-night-light-fault"))
control = importlib.util.module_from_spec(importlib.util.spec_from_loader(loader.name, loader))
loader.exec_module(control)


class FaultAdmission(unittest.TestCase):
    def baseline(self):
        return {"locked": False, "night_light": {"supported": True, "service_supported": True, "temperature": 3700,
                "service_rgb_scales": [1.0, .7, .3], "owner_epoch": 4, "generation": 9,
                "outputs": [{"name": "nested", "physical_size": [1280, 800], "location": [0, 0], "scale": 1.0,
                             "last_submitted_transform": [4, 9, 1, [1.0, .7, .3]]}]}}

    def test_unlocked_original_current_warm_submission_is_required(self):
        self.assertTrue(control.warm_submission(self.baseline()))
        for field, value in [("locked", True), ("locked", None)]:
            actual = self.baseline(); actual[field] = value
            self.assertFalse(control.warm_submission(actual))
        actual = self.baseline(); actual["night_light"]["supported"] = False
        self.assertFalse(control.warm_submission(actual))

    def test_stale_or_missing_original_submission_is_rejected(self):
        for submitted in [[3, 9, 1, [1, .7, .3]], [4, 8, 1, [1, .7, .3]],
                          [True, 9, 1, [1, .7, .3]], None, [4, 9]]:
            actual = self.baseline(); actual["night_light"]["outputs"][0]["last_submitted_transform"] = submitted
            self.assertFalse(control.warm_submission(actual))

    def test_neutral_malformed_and_nonfinite_values_are_rejected(self):
        for rgb in [[1, 1, 1], [1, float("nan"), .3], [1, float("inf"), .3], [1, -.1, .3],
                    [1, 1.01, .3], [True, .7, .3], "private malformed payload"]:
            actual = self.baseline(); actual["night_light"]["outputs"][0]["last_submitted_transform"][3] = rgb
            self.assertFalse(control.warm_submission(actual))

    def test_every_bounded_output_needs_original_warm_submission(self):
        actual = self.baseline(); actual["night_light"]["outputs"] = []
        self.assertFalse(control.warm_submission(actual))
        actual = self.baseline(); actual["night_light"]["outputs"] *= 9
        self.assertFalse(control.warm_submission(actual))
        actual = self.baseline(); actual["night_light"]["outputs"].append({"last_submitted_transform": None})
        self.assertFalse(control.warm_submission(actual))

    def test_rpm_owner_and_complete_verification_must_both_succeed(self):
        def result(code=0, out=b"", err=b""):
            return subprocess.CompletedProcess([], code, out, err)
        owner = result(out=b"roost-night-light-fixture\n")
        self.assertTrue(control.valid_rpm_receipt(owner, result()))
        for rejected in [result(1, owner.stdout), result(out=b"roost-compositor\n"),
                         result(out=owner.stdout * 2), result(out=owner.stdout, err=b"warning")]:
            self.assertFalse(control.valid_rpm_receipt(rejected, result()))
        for rejected in [result(1), result(out=b"S.5....T. fixture"), result(err=b"warning")]:
            self.assertFalse(control.valid_rpm_receipt(owner, rejected))

    def test_actual_rpm_commands_and_failed_verification_never_fallback(self):
        owner = subprocess.CompletedProcess([], 0, b"roost-night-light-fixture\n", b"")
        for code in (0, 1):
            verified = subprocess.CompletedProcess([], code, b"", b"")
            with mock.patch.object(control.pathlib.Path, "is_file", return_value=True), \
                    mock.patch.object(control.subprocess, "run", side_effect=[owner, verified]) as run:
                if code:
                    with self.assertRaises(RuntimeError):
                        control.fixture_package()
                else:
                    receipt = control.fixture_package()
                    self.assertEqual(receipt["manager"], "rpm")
                    self.assertIs(receipt["verified"], True)
                self.assertEqual(run.call_count, 2)
                self.assertEqual(run.call_args_list[0].args[0],
                    ["/usr/bin/rpm", "-qf", "--queryformat", "%{NAME}\\n", str(control.EXE)])
                self.assertEqual(run.call_args_list[1].args[0], ["/usr/bin/rpm", "-Vf", str(control.EXE)])

    def test_original_state_fd_hash_and_named_identity_are_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            raw = json.dumps(self.baseline()).encode()
            path.write_bytes(raw); path.chmod(0o600)
            receipt = control.read_state_receipt(path)
            self.assertEqual(receipt["sha256"], hashlib.sha256(raw).hexdigest())
            self.assertEqual(receipt["inode"], path.stat().st_ino)
            self.assertEqual(receipt["size"], len(raw))
            self.assertEqual(receipt["uid"], 1000)

    def test_replaced_named_state_after_original_read_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            path.write_text(json.dumps(self.baseline())); path.chmod(0o600)
            original_fdopen = control.os.fdopen
            class ReplacingRead:
                def __init__(self, stream): self.stream = stream
                def __enter__(self): return self
                def __exit__(self, *args): self.stream.close()
                def fileno(self): return self.stream.fileno()
                def read(self, limit):
                    raw = self.stream.read(limit)
                    replacement = path.with_suffix(".replacement")
                    replacement.write_bytes(raw); replacement.chmod(0o600)
                    replacement.replace(path)
                    return raw
            with mock.patch.object(control.os, "fdopen", side_effect=lambda *args: ReplacingRead(original_fdopen(*args))):
                with self.assertRaises(RuntimeError):
                    control.read_state_receipt(path)

    def test_oversize_state_refused_before_read_or_json_decode(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            with path.open("wb") as stream: stream.truncate(1024 * 1024 + 1)
            path.chmod(0o600)
            with mock.patch.object(control.json, "loads") as decode:
                with self.assertRaises(RuntimeError): control.read_state_receipt(path)
                decode.assert_not_called()

    def test_symlink_state_is_not_an_original_file_receipt(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            path.write_text(json.dumps(self.baseline())); path.chmod(0o600)
            link = Path(directory) / "link.json"; link.symlink_to(path)
            with self.assertRaises(OSError): control.read_state_receipt(link)

    def test_route_admission_checks_only_exact_original_public_environment_key(self):
        for raw, allowed in [(b"PRIVATE_TOKEN=do-not-retain\0ROOST_COMPOSITOR_STATE=/out/state.json\0", True),
                             (b"PRIVATE_TOKEN=do-not-retain\0", False),
                             (b"ROOST_COMPOSITOR_STATE=/other/state.json\0", False),
                             (b"ROOST_COMPOSITOR_STATE=/out/state.json\0" * 2, False),
                             (b"x" * 65537, False)]:
            with mock.patch.object(control.pathlib.Path, "open", return_value=io.BytesIO(raw)):
                if allowed:
                    control.require_original_state_route(42, "/out/state.json")
                else:
                    with self.assertRaises(RuntimeError) as raised:
                        control.require_original_state_route(42, "/out/state.json")
                    self.assertNotIn("do-not-retain", str(raised.exception))
                    self.assertNotIn("/other", str(raised.exception))

    def test_admitted_summary_preserves_all_semantic_fields_and_excludes_unrelated_counters(self):
        baseline = control.transform_summary(self.baseline())
        value = self.baseline(); value["frame_counter"] = 1000
        value["night_light"]["outputs"][0]["stage_counters"] = [7, 0]
        self.assertEqual(control.transform_summary(value), baseline)
        self.assertEqual(set(baseline), {"owner_epoch", "generation", "temperature", "service_rgb_scales", "outputs"})
        self.assertEqual(set(baseline["outputs"][0]),
            {"name", "physical_size", "location", "scale", "last_submitted_transform"})
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "state.json"
            path.write_text(json.dumps(value)); path.chmod(0o600)
            with mock.patch.object(control.time, "time_ns", return_value=123456789):
                receipt = control.read_state_receipt(path)
            self.assertEqual(receipt["transform"], baseline)
            self.assertEqual(receipt["admitted_wall_ns"], 123456789)

    def test_transform_summary_rejects_unbounded_or_unsubmitted_public_fields(self):
        for key, replacement in [("name", "private description with spaces"), ("physical_size", [8193, 800]),
                                 ("location", [2**31, 0]), ("scale", float("nan")),
                                 ("last_submitted_transform", [4, 9, 1, [1., .8, .3]])]:
            value = self.baseline(); value["night_light"]["outputs"][0][key] = replacement
            with self.assertRaises(RuntimeError): control.transform_summary(value)
        value = self.baseline(); value["night_light"]["outputs"] *= 2
        with self.assertRaises(RuntimeError): control.transform_summary(value)

    def test_actual_main_rejects_changed_or_expired_baseline_before_any_marker_create(self):
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory) / "state.json"
            state.write_text(json.dumps(self.baseline())); state.chmod(0o600)
            request_path = Path(directory) / "fault-request.json"
            original = {"pid": os.getpid(), "start_ticks": 10, "exe_sha256": "a" * 64}
            stamp = 100_000_000_000
            baseline = {"sha256": "b" * 64, "dev": 1, "inode": 2, "size": 100,
                        "mtime_ns": 1, "ctime_ns": 1, "observed_wall_ns": stamp - 1}
            for reason in ("changed", "expired", "future"):
                transform = control.transform_summary(self.baseline())
                receipt = dict(baseline)
                if reason == "changed": transform["outputs"][0]["scale"] = 2.0
                elif reason == "expired": receipt["observed_wall_ns"] = stamp - 5_000_000_001
                else: receipt["observed_wall_ns"] = stamp + 1
                request_path.write_text(json.dumps({"pid": original["pid"], "start": 10, "exe_sha256": "a" * 64,
                    "baseline_receipt": receipt, "baseline_transform": transform})); request_path.chmod(0o600)
                with mock.patch.object(control.os, "geteuid", return_value=0), \
                        mock.patch.object(control, "original_identity", return_value=original), \
                        mock.patch.object(control, "fixture_package", return_value={"manager": "rpm"}), \
                        mock.patch.object(control, "require_original_state_route"), \
                        mock.patch.object(control.time, "time_ns", return_value=stamp), \
                        mock.patch.object(control.subprocess, "run", return_value=subprocess.CompletedProcess([], 0, b"version [night-light-vm-fixture]", b"")), \
                        mock.patch.object(control.os, "open", wraps=os.open) as opened:
                    with self.assertRaisesRegex(RuntimeError, "differs" if reason == "changed" else "clock"):
                        control.main(original["pid"], 10, "a" * 64, str(state), str(request_path))
                    marker = Path(f"/run/roost-vm-night-light-fault-{original['pid']}")
                    self.assertFalse(marker.exists())
                    self.assertFalse(any(Path(call.args[0]) == marker for call in opened.call_args_list))

    def test_baseline_admission_retains_distinct_original_receipts_without_counter_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "request.json"
            transform = control.transform_summary(self.baseline())
            baseline = {"sha256": "b" * 64, "dev": 1, "inode": 2, "size": 100,
                        "mtime_ns": 1, "ctime_ns": 1, "observed_wall_ns": 1}
            path.write_text(json.dumps({"pid": 42, "start": 10, "exe_sha256": "a" * 64,
                "baseline_receipt": baseline, "baseline_transform": transform})); path.chmod(0o600)
            admitted = {"transform": transform, "sha256": "c" * 64, "inode": 3}
            with mock.patch.object(control.time, "time_ns", return_value=5_000_000_001):
                result = control.admit_baseline(path, {"pid": 42, "start_ticks": 10, "exe_sha256": "a" * 64}, admitted)
            self.assertEqual(result["capture_to_admission_ns"], 5_000_000_000)
            self.assertEqual(result["baseline_receipt"], baseline)
            self.assertNotEqual(result["baseline_receipt"]["sha256"], admitted["sha256"])
            self.assertFalse(result["same_snapshot_required"])

    def test_injection_is_absent_from_default_feature_set(self):
        package = tomllib.loads((ROOT / "crates/compositor/Cargo.toml").read_text())
        self.assertNotIn("night-light-vm-fixture", package["features"]["default"])
        declarations = (ROOT / "crates/compositor/src/lib.rs").read_text()
        self.assertIn('#[cfg(feature = "night-light-vm-fixture")]\nmod night_light_fixture;', declarations)

    def test_actual_shipping_guard_rejects_fixture_even_without_check_step(self):
        source = (ROOT / "packaging/arch/PKGBUILD").read_text()
        function = source[source.index("_assert_production_compositor() {"):source.index("\ncheck() {")]
        self.assertIn("package() {\n    _assert_production_compositor || return 1", source)
        with tempfile.TemporaryDirectory() as directory:
            release = Path(directory) / "release"; release.mkdir()
            binary = release / "roost-compositor"
            for fixture in (False, True):
                text = "roost-compositor 0.1.0" + (" [night-light-vm-fixture]" if fixture else "")
                binary.write_text("#!/bin/sh\nprintf '%s\\n' '" + text + "'\n"); binary.chmod(0o755)
                actual = subprocess.run(["bash", "-c", function + "\n_assert_production_compositor"],
                    env={**os.environ, "CARGO_TARGET_DIR": directory}, capture_output=True)
                self.assertEqual(actual.returncode, 1 if fixture else 0)


if __name__ == "__main__":
    unittest.main()
