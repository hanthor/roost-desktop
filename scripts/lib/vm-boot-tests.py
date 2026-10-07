#!/usr/bin/env python3
"""Protect the shared boot API used by the independent performance lane."""
import importlib.machinery
import importlib.util
import io
import ast
import base64
import json
import os
import subprocess
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from types import SimpleNamespace

loader = importlib.machinery.SourceFileLoader("vm_boot_lane", str(Path(__file__).resolve().parents[1] / "roost-vm-lane"))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


class OrcaCommandWindow(unittest.TestCase):
    def check(self, started=10.0, ready=10.5, before=11.0, after=11.1, samples=96000):
        return lane.require_speech_trigger_window({"receipt": {"started_monotonic": started,
            "ready_monotonic": ready}}, {"guest_monotonic": before}, {"guest_monotonic": after}, samples)
    def test_original_trigger_in_actual_bounded_pcm_window(self):
        self.check()
    def test_delayed_trigger_and_exact_end_refused_without_padding(self):
        for before, after in ((14.0, 14.1), (13.9, 14.0), (11.1, 11.0), (10.4, 11.0)):
            with self.subTest(before=before, after=after), self.assertRaises(RuntimeError):
                self.check(before=before, after=after)
    def test_finite_original_receipts_and_sample_bound_required(self):
        for kwargs in ({"started": float("nan")}, {"ready": float("inf")}, {"before": True},
                       {"after": -1}, {"samples": 96001}, {"samples": True}, {"ready": 9.9}):
            with self.subTest(kwargs=kwargs), self.assertRaises(RuntimeError): self.check(**kwargs)
    def test_actual_command_calls_window_validator_with_original_receipts(self):
        node = next(n for n in ast.walk(ast.parse(Path(lane.__file__).read_text()))
                    if isinstance(n, ast.FunctionDef) and n.name == "native_command")
        call = next(n for n in ast.walk(node) if isinstance(n, ast.Call)
                    and isinstance(n.func, ast.Name) and n.func.id == "require_speech_trigger_window")
        self.assertEqual([ast.unparse(n) for n in call.args],
                         ["start_speech", "trigger_before", "trigger_after", "len(values)"])


class OrcaPublicWindow(unittest.TestCase):
    def receipts(self):
        profile = {"name": "public-navigation", "rate": 24000, "channels": 1,
                   "sample_width": 2, "sample_count": 192000, "runtime_max_sec": 12}
        return ({"receipt": {"profile": dict(profile), "started_monotonic": 241.86064434,
                             "ready_monotonic": 242.236375991}},
                {"receipt": {"profile": dict(profile)}},
                {"guest_monotonic": 243.524247449}, {"guest_monotonic": 247.041191809})
    def test_original_actual_full_navigation_bracket_needs_actual_samples(self):
        args = self.receipts()
        lane.require_public_speech_window(*args, 192000)
        with self.assertRaises(RuntimeError): lane.require_public_speech_window(*args, 96000)
    def test_exact_actual_end_delayed_reversed_or_preready_trigger_rejected(self):
        for before, after in ((243, 249.86064434), (243, 250), (244, 243), (242, 243)):
            args = self.receipts()
            args[2]["guest_monotonic"] = before; args[3]["guest_monotonic"] = after
            with self.subTest(before=before, after=after), self.assertRaises(RuntimeError):
                lane.require_public_speech_window(*args, 192000)
    def test_finite_samples_and_original_matching_profile_required(self):
        for key, value in (("rate", True), ("sample_count", 192001), ("name", "native-command"),
                           ("channels", 2), ("runtime_max_sec", 13)):
            args = self.receipts(); args[1]["receipt"]["profile"][key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                lane.require_public_speech_window(*args, 192000)
        for samples in (True, -1, 192001, 192000.0):
            with self.subTest(samples=samples), self.assertRaises(RuntimeError):
                lane.require_public_speech_window(*self.receipts(), samples)
        for value in (True, float("nan"), float("inf"), -1):
            args = self.receipts(); args[3]["guest_monotonic"] = value
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                lane.require_public_speech_window(*args, 192000)
    def test_guest_fixed_profiles_and_changed_recording_refused(self):
        source = Path(lane.__file__).parents[1] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        nodes = [n for n in ast.parse(source.read_text()).body if isinstance(n, ast.FunctionDef)
                 and n.name in {"speech_capture_profile", "recording_profile"}]
        scope = {}; exec(compile(ast.Module(body=nodes, type_ignores=[]), "actual-profile", "exec"), scope)
        for name, count, limit in (("native-command", 96000, 8), ("public-navigation", 192000, 12)):
            profile = scope["speech_capture_profile"](name)
            self.assertEqual((profile["sample_count"], profile["runtime_max_sec"]), (count, limit))
            self.assertEqual(scope["recording_profile"]({"profile": profile}), profile)
            with self.assertRaises(RuntimeError):
                scope["recording_profile"]({"profile": dict(profile, channels=True)})
        for name in (True, [], "private", "public-navigation-extra"):
            with self.assertRaises(RuntimeError): scope["speech_capture_profile"](name)
    def test_public_profile_only_and_original_guard_before_lock_privacy(self):
        tree = ast.parse(Path(lane.__file__).read_text())
        public = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "public_capture")
        native = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "native_command")
        starts = lambda node: [n for n in ast.walk(node) if isinstance(n, ast.Call)
                and isinstance(n.func, ast.Attribute) and n.func.attr == "run" and n.args
                and isinstance(n.args[0], ast.Constant) and n.args[0].value == "orca-speech-start"]
        self.assertEqual([ast.literal_eval(a) for a in starts(public)[0].args],
                         ["orca-speech-start", "public-navigation"])
        self.assertEqual([ast.literal_eval(a) for a in starts(native)[0].args], ["orca-speech-start"])
        validator = next(n for n in ast.walk(public) if isinstance(n, ast.Call)
                         and isinstance(n.func, ast.Name) and n.func.id == "require_public_speech_window")
        self.assertEqual([ast.unparse(a) for a in validator.args],
                         ["start", "end", "before_trigger", "after_trigger", "len(samples)"])
        cleanup = next(n for n in ast.walk(public) if isinstance(n, ast.Try) and n.finalbody)
        self.assertTrue(any(isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute)
                            and n.func.attr == 'run' and n.args and isinstance(n.args[0], ast.Constant)
                            and n.args[0].value == 'orca-speech-end' for n in ast.walk(cleanup.finalbody[0])))
        source = Path(lane.__file__).read_text()
        self.assertLess(source.index('public_capture("orca-quick-settings-volume"'), source.index('def pam_unlock('))


class OrcaCommandCleanup(unittest.TestCase):
    def exercise(self, failure, cleanup_failure=False):
        source = Path(lane.__file__).read_text()
        node = next(n for n in ast.walk(ast.parse(source))
                    if isinstance(n, ast.FunctionDef) and n.name == "native_command")
        calls = []
        original = RuntimeError("controlled original command failure")
        secondary = ValueError("controlled cleanup failure")
        class Agent:
            def run(self, action):
                calls.append(action)
                if action == "orca-speech-start":
                    if failure == "start": raise original
                    return {}
                if action == "orca-speech-end":
                    if cleanup_failure: raise secondary
                    return {}
                if action == "orca-speech-trigger" and failure == "trigger": raise original
                if action == "orca-read":
                    if failure == "read": raise original
                    return {"keyboard": {"grab_all": failure != "grab"}}
                return {}
        def keys(*args):
            if failure == "keys": raise original
        with tempfile.TemporaryDirectory() as temporary:
            scope = {"agent": Agent(), "qmp": SimpleNamespace(keys=keys),
                     "time": SimpleNamespace(sleep=lambda _: None), "os": os,
                     "json": json, "evidence": temporary}
            exec(compile(ast.Module(body=[node], type_ignores=[]), "actual-native-command", "exec"), scope)
            with self.assertRaises(Exception) as raised:
                scope["native_command"]("controlled", "controlled-test")
            if failure is None: self.assertIs(raised.exception, secondary)
            elif failure != "grab": self.assertIs(raised.exception, original)
            else: self.assertIsInstance(raised.exception, RuntimeError)
            self.assertEqual(calls.count("orca-speech-end"), 0 if failure == "start" else 1)
            receipt = Path(temporary, "controlled-failure.json")
            if failure == "start": self.assertFalse(receipt.exists())
            else:
                self.assertEqual(json.loads(receipt.read_text()), {
                    "primary_error_type": "RuntimeError" if failure is not None else None,
                    "cleanup_error_type": "ValueError" if cleanup_failure else None})
                self.assertNotIn("controlled original", receipt.read_text())
    def test_actual_midcommand_failures_close_original_recording_once(self):
        for failure in ("trigger", "keys", "read", "grab"):
            with self.subTest(failure=failure): self.exercise(failure)
    def test_actual_cleanup_failure_preserves_original_command_error(self):
        self.exercise("keys", cleanup_failure=True)
    def test_actual_cleanup_only_failure_remains_failed(self):
        self.exercise(None, cleanup_failure=True)
    def test_actual_start_failure_uses_existing_start_cleanup_only(self):
        self.exercise("start")


class OrcaFixturePackages(unittest.TestCase):
    def test_actual_upstream_export_create_requires_explicit_command(self):
        source = Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml"
        lines = [line.strip() for line in source.read_text().splitlines()
                 if "probe_container=$(sudo podman create" in line]
        self.assertEqual(len(lines), 1)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sudo = root / "sudo"
            sudo.write_text('#!/bin/sh\nexec "$@"\n')
            sudo.chmod(0o755)
            podman = root / "podman"
            podman.write_text("#!/usr/bin/env python3\nimport sys\n"
                "assert sys.argv[1:] == ['create','localhost/marlin-roost-vm:ci','/usr/bin/true']\n"
                "print('controlled-inspect-container')\n")
            podman.chmod(0o755)
            environment = {**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"]}
            for line, success in [(lines[0], True), (lines[0].replace(" /usr/bin/true", ""), False)]:
                result = subprocess.run(["bash", "-eu", "-c", line + '\nprintf "%s\\n" "$probe_container"\n'],
                    env=environment, capture_output=True, text=True, timeout=10)
                if success:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout.strip(), "controlled-inspect-container")
                else:
                    self.assertNotEqual(result.returncode, 0)
        # Controlled shell admission only; no Podman image/runtime proof.

    def test_actual_fixture_session_fixes_inherited_locale_before_child_exec(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-session"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            command = root / "systemd-cat"
            command.write_text("#!/usr/bin/env python3\nimport json,os\nprint(json.dumps({k:os.environ[k] for k in ['LC_ALL','LANG','LANGUAGE','XDG_SESSION_TYPE']}))\n")
            command.chmod(0o755)
            result = subprocess.run(["bash", str(source)], env={**os.environ,
                "PATH": str(root) + os.pathsep + os.environ["PATH"], "XDG_RUNTIME_DIR": str(root),
                "LC_ALL": "C", "LANG": "fr_FR.UTF-8", "LANGUAGE": "fr"},
                capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), {"LC_ALL": "C.UTF-8", "LANG": "C.UTF-8",
                "LANGUAGE": "C", "XDG_SESSION_TYPE": "wayland"})
            self.assertTrue((root / "roost-vm-orca-token").is_file())
        # Fixture setup policy only, not a genuine reader or translated-label proof.

    def run_builder_admission(self, upgraded="2.58.9", failed=False):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/orca51/build"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "bin"
            binary.mkdir()
            stub = """#!/usr/bin/env python3
import json,os,pathlib,sys
name=pathlib.Path(sys.argv[0]).name
args=sys.argv[1:]
with open(os.environ['CALLS'],'a') as stream: stream.write(json.dumps([name,*args])+'\\n')
if name=='pacman':
 if args and args[0]=='-Syu':
  if os.environ['FAIL_UPGRADE']=='true': sys.exit(7)
  pathlib.Path(os.environ['UPGRADED']).touch()
 elif args==['-Q','at-spi2-core']:
  print('at-spi2-core '+(os.environ['NEW_ATSPI'] if pathlib.Path(os.environ['UPGRADED']).exists() else '2.58.5'))
 elif args==['-Q','orca']: print('orca 50.3')
elif name=='vercmp':
 assert args==[os.environ['NEW_ATSPI'],'2.58.6']
 print(-1 if tuple(map(int,args[0].split('.'))) < (2,58,6) else 0)
"""
            for name in ("pacman", "useradd", "install", "chown", "runuser", "cp", "vercmp"):
                command = binary / name
                command.write_text(stub)
                command.chmod(0o755)
            # Execute the unchanged production admission/decision block. Only its
            # fixed artifact root is relocated; every mutating command is a stub.
            body = source.read_text().split("# A sufficiently new", 1)[0]
            body = body.replace("/orca51-artifacts", str(root / "artifacts"))
            result = subprocess.run(["bash", "-c", body], env={**os.environ,
                "PATH": str(binary) + os.pathsep + os.environ["PATH"], "CALLS": str(root / "calls"),
                "UPGRADED": str(root / "upgraded"), "NEW_ATSPI": upgraded,
                "FAIL_UPGRADE": "true" if failed else "false"}, capture_output=True, text=True, timeout=10)
            calls = [json.loads(line) for line in (root / "calls").read_text().splitlines()]
            receipts = {p.name: p.read_text().strip() for p in (root / "artifacts").iterdir()}
            return result, calls, receipts

    def test_builder_decides_atspi_after_coherent_upgrade_and_never_downgrades(self):
        result, calls, receipts = self.run_builder_admission()
        self.assertEqual(result.returncode, 0, result.stderr)
        upgrades = [call for call in calls if call[:2] == ["pacman", "-Syu"]]
        self.assertEqual(len(upgrades), 1)
        self.assertFalse(any(call[:2] == ["pacman", "-Sy"] for call in calls))
        self.assertEqual(receipts["base-atspi.txt"], "at-spi2-core 2.58.5")
        self.assertEqual(receipts["coherent-atspi.txt"], "at-spi2-core 2.58.9")
        self.assertIn(["vercmp", "2.58.9", "2.58.6"], calls)
        self.assertFalse(any(call[0] == "runuser" for call in calls))
        self.assertFalse(any(call[:2] == ["pacman", "-U"] for call in calls))
        result, calls, _ = self.run_builder_admission("2.58.5")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(any(call[0] == "runuser" for call in calls))
        self.assertTrue(any(call[:2] == ["pacman", "-U"] for call in calls))

    def test_failed_coherent_upgrade_stops_before_fresh_decision_or_package_build(self):
        result, calls, receipts = self.run_builder_admission(failed=True)
        self.assertEqual(result.returncode, 7)
        self.assertEqual(sum(call == ["pacman", "-Q", "at-spi2-core"] for call in calls), 1)
        self.assertNotIn("coherent-atspi.txt", receipts)
        self.assertFalse(any(call[0] in ("useradd", "runuser", "vercmp") for call in calls))

    def run_runtime_admission(self, upgraded="2.60.0", failed=False, query_failed=False):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/orca51/Containerfile"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "bin"
            binary.mkdir()
            stub = """#!/usr/bin/env python3
import json,os,pathlib,sys
name=pathlib.Path(sys.argv[0]).name
args=sys.argv[1:]
with open(os.environ['CALLS'],'a') as stream: stream.write(json.dumps([name,*args])+'\\n')
if name=='pacman':
 if args and args[0]=='-Syu':
  if os.environ['FAIL_UPGRADE']=='true': sys.exit(7)
  pathlib.Path(os.environ['UPGRADED']).touch()
 elif args==['-Q','at-spi2-core']:
  if os.environ.get('FAIL_QUERY')=='true': sys.exit(9)
  print('at-spi2-core '+(os.environ['NEW_ATSPI'] if pathlib.Path(os.environ['UPGRADED']).exists() else '2.58.5'))
 elif args==['-Q','orca']: print('orca 50.3')
elif name=='vercmp':
 assert args==[os.environ['NEW_ATSPI'],'2.58.6']
 print(-1 if tuple(map(int,args[0].split('.'))) < (2,58,6) else 0)
"""
            for name in ("pacman", "vercmp", "roost-image-kernels"):
                command = binary / name
                command.write_text(stub)
                command.chmod(0o755)
            # Execute the actual final-stage transaction through kernel validation.
            # Mutators are stubs; fixed paths are only inert package arguments.
            body = source.read_text().split("RUN set -eu;", 1)[1].split("&& pacman -Qi", 1)[0]
            body = body.replace("\\\n", " ").strip()
            body = body.replace("/usr/libexec/roost-image-kernels", str(binary / "roost-image-kernels"))
            result = subprocess.run(["bash", "-euo", "pipefail", "-c", body], env={**os.environ,
                "PATH": str(binary) + os.pathsep + os.environ["PATH"], "CALLS": str(root / "calls"),
                "UPGRADED": str(root / "upgraded"), "NEW_ATSPI": upgraded,
                "FAIL_UPGRADE": "true" if failed else "false", "FAIL_QUERY": "true" if query_failed else "false"}, capture_output=True, text=True, timeout=10)
            calls = [json.loads(line) for line in (root / "calls").read_text().splitlines()]
            return result, calls

    def test_actual_runtime_transaction_does_not_downgrade_sufficient_atspi(self):
        result, calls = self.run_runtime_admission()
        self.assertEqual(result.returncode, 0, result.stderr)
        install = next(call for call in calls if call[:2] == ["pacman", "-U"])
        self.assertEqual(len(install), 4)
        self.assertTrue(install[-1].endswith("/orca-*.pkg.tar.zst"))
        self.assertEqual(calls[-1], ["roost-image-kernels", "vm"])
        result, calls = self.run_runtime_admission("2.58.5")
        self.assertEqual(result.returncode, 0, result.stderr)
        install = next(call for call in calls if call[:2] == ["pacman", "-U"])
        self.assertEqual(len(install), 5)
        self.assertTrue(install[-1].endswith("/at-spi2-core-*.pkg.tar.zst"))
        result, calls = self.run_runtime_admission(failed=True)
        self.assertEqual(result.returncode, 7)
        self.assertFalse(any(call[:2] == ["pacman", "-U"] or call[0] == "roost-image-kernels" for call in calls))
        result, calls = self.run_runtime_admission(query_failed=True)
        self.assertEqual(result.returncode, 9)
        self.assertFalse(any(call[:2] == ["pacman", "-U"] or call[0] == "roost-image-kernels" for call in calls))
        # Source execution with stubs, not an actual dependency/kernel qualification.

    def test_separate_runtime_upgrade_and_package_install_precede_kernel_validation(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/orca51/Containerfile"
        self.assertIn('SHELL ["/bin/bash", "-o", "pipefail", "-c"]', source.read_text())
        body = source.read_text().split("RUN set -eu; pacman -Syu", 1)[1]
        self.assertLess(body.index("pacman -U"), body.index("roost-image-kernels vm"))
        self.assertLess(body.index("roost-image-kernels vm"), body.index("pacman -Qi orca"))
        helper = (source.parents[2] / "roost-image-kernels").read_text()
        self.assertIn('case "$stage" in preview|vm)', helper)
        self.assertNotIn("pacman -Sy --", source.read_text())
        # Source ordering only; actual package graph/kernel proof remains CI/VM.


class OrcaReadFailure(unittest.TestCase):
    def test_failed_observation_emits_safe_receipt_and_keeps_exit_one(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        main = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "main")
        branch = next(n for n in ast.walk(main) if isinstance(n, ast.If)
                      and isinstance(n.test, ast.Compare) and any(isinstance(v, ast.Constant) and v.value == "orca-read"
                          for v in n.test.comparators))
        def observe(): raise RuntimeError("private-error-text")
        scope = {"orca_observation": observe, "orca_read_error": lambda error: {"exception_type": type(error).__name__}, "json": json}
        with patch("sys.stdout", new_callable=io.StringIO) as out:
            with self.assertRaises(SystemExit) as caught:
                exec(compile(ast.Module(body=branch.body, type_ignores=[]), str(source), "exec"), scope)
        self.assertEqual(caught.exception.code, 1)
        self.assertTrue(caught.exception.__suppress_context__)
        self.assertEqual(json.loads(out.getvalue()), {"orca_read_error": {"exception_type": "RuntimeError"}})
        self.assertNotIn("private-error-text", out.getvalue())

    def test_failed_read_rejects_arbitrary_private_or_invalid_schema(self):
        actor = lane.guest_agent.GuestAgent.__new__(lane.guest_agent.GuestAgent)
        invalid = [{"exception_type": "private-token"},
                   {"exception_type": "CalledProcessError", "argv": "private-token"},
                   {"exception_type": "CalledProcessError", "returncode": True},
                   {"exception_type": "CalledProcessError", "returncode": 256},
                   {"exception_type": "RuntimeError", "returncode": 1},
                   {"exception_type": "RuntimeError", "errno": 1},
                   {"exception_type": "CalledProcessError", "returncode": 1,
                    "dbus_method": "SetLogFileForTesting", "stderr": "private-token"},
                   {"exception_type": "CalledProcessError", "returncode": 1,
                    "dbus_method": "GetVersion", "stderr": "x" * 513},
                   {"exception_type": "CalledProcessError", "returncode": 1,
                    "dbus_method": "GetVersion", "stderr": "\x00private-token"}]
        for value in invalid:
            output = base64.b64encode(json.dumps({"orca_read_error": value}).encode()).decode()
            failure = {"exited": True, "exitcode": 1, "out-data": output}
            with patch.object(actor, "command", side_effect=[{"pid": 10}, failure]):
                with self.assertRaisesRegex(RuntimeError, r"guest lifecycle orca-read failed \(exit=1\)") as caught:
                    actor.run("orca-read")
            self.assertNotIn("private-token", str(caught.exception))
        # Schema/privacy policy only; the retained real VM failure remains unexplained.

    def test_read_diagnostics_preserve_only_fixed_public_dbus_failure(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "orca_read_error")
        scope = {"subprocess": lane.subprocess}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        diagnose = scope["orca_read_error"]
        command = ["runuser", "-u", "roost-test", "--", "env", "PRIVATE_ENV=do-not-copy",
                   "busctl", "--user", "call", "org.freedesktop.DBus", "/org/freedesktop/DBus",
                   "org.freedesktop.DBus", "GetConnectionUnixProcessID", "s", ":1.527"]
        error = lane.subprocess.CalledProcessError(1, command, stderr="Call failed: owner vanished\n" + "x" * 1024)
        result = diagnose(error)
        self.assertEqual(result["dbus_method"], "GetConnectionUnixProcessID")
        self.assertEqual(result["returncode"], 1)
        self.assertEqual(len(result["stderr"]), 512)
        self.assertNotIn("PRIVATE_ENV", json.dumps(result))
        for unsafe in (["orca", "--testing-token=do-not-copy"],
                       [*command[:-3], "SetLogFileForTesting", "ss", "do-not-copy"]):
            result = diagnose(lane.subprocess.CalledProcessError(1, unsafe, stderr="do-not-copy"))
            self.assertNotIn("stderr", result)
            self.assertNotIn("do-not-copy", json.dumps(result))
        self.assertEqual(diagnose(FileNotFoundError(2, "do-not-copy")),
                         {"exception_type": "FileNotFoundError", "errno": 2})
    def test_guest_transport_keeps_read_failure_and_other_action_output_private(self):
        diagnostic = {"orca_read_error": {"exception_type": "CalledProcessError",
                      "returncode": 1, "dbus_method": "GetVersion", "stderr": "Call failed: owner vanished"}}
        failed = {"exited": True, "exitcode": 1,
                  "out-data": base64.b64encode(json.dumps(diagnostic).encode()).decode(),
                  "err-data": base64.b64encode(b"private arbitrary traceback").decode()}
        actor = lane.guest_agent.GuestAgent.__new__(lane.guest_agent.GuestAgent)
        for action in ("orca-read", "orca-speech-start"):
            with patch.object(actor, "command", side_effect=[{"pid": 10}, failed]):
                with self.assertRaises(RuntimeError) as caught:
                    actor.run(action)
            message = str(caught.exception)
            self.assertNotIn("private arbitrary traceback", message)
            if action == "orca-read":
                self.assertIn("owner vanished", message)
                self.assertIn("failed (exit=1)", message)
            else:
                self.assertNotIn("owner vanished", message)
        malformed = ["!", base64.b64encode(b"\xff").decode(),
                     base64.b64encode(b"[").decode(), base64.b64encode(b"[]").decode(),
                     base64.b64encode(b"x" * 4097).decode(), "A" * 5465, None, ["not text"]]
        for output in malformed:
            with patch.object(actor, "command", side_effect=[{"pid": 10}, {**failed, "out-data": output}]):
                with self.assertRaisesRegex(RuntimeError, r"guest lifecycle orca-read failed \(exit=1\)"):
                    actor.run("orca-read")


class ConstraintProofCleanup(unittest.TestCase):
    def test_original_error_survives_all_independent_cleanup_failures(self):
        calls = []
        def run(action):
            calls.append(action)
            raise RuntimeError("failed " + action)
        def restore(*arguments):
            calls.append("restore")
            raise RuntimeError("failed restoration")
        original = RuntimeError("original admission failure")
        with tempfile.TemporaryDirectory() as out, patch.object(lane, "restore_corner_mouse", side_effect=restore):
            with self.assertRaises(RuntimeError) as caught:
                try:
                    raise original
                finally:
                    lane.finish_constraint_proof(SimpleNamespace(run=run), object(), {}, out,
                        {"principal": None, "primary_error": {"message": str(original)}})
            self.assertIs(caught.exception, original)
            self.assertEqual(calls, ["constraint-quit", "restore", "constraint-log"])
            actual = json.loads((Path(out) / "hot-corner-constraints.json").read_text())
            self.assertIsNone(actual["principal"])
            self.assertEqual(actual["primary_error"]["message"], str(original))
            self.assertEqual([error["action"] for error in actual["cleanup_errors"]],
                             ["constraint-quit", "original-mouse-restoration", "constraint-log-retention"])

    def test_cleanup_only_failure_rejects_instead_of_qualifying(self):
        def run(action):
            if action == "constraint-quit":
                raise RuntimeError("original principal quit failed")
            return {"stderr": "original client log", "truncated": False}
        with tempfile.TemporaryDirectory() as out, patch.object(lane, "restore_corner_mouse", return_value={"verified": True}):
            with self.assertRaisesRegex(RuntimeError, "actual constraint cleanup failed"):
                lane.finish_constraint_proof(SimpleNamespace(run=run), object(), {}, out,
                    {"principal": {"pid": 42}, "primary_error": None})
            actual = json.loads((Path(out) / "hot-corner-constraints.json").read_text())
            self.assertIsNone(actual["primary_error"])
            self.assertEqual(actual["cleanup_errors"][0]["action"], "constraint-quit")
            self.assertTrue((Path(out) / "hot-corner-constraint-mouse-restoration.json").exists())
            self.assertTrue((Path(out) / "hot-corner-constraint-stderr.json").exists())
class PublicOrcaNavigation(unittest.TestCase):
    def focus_module(self):
        file = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-shell-focus"
        loader = importlib.machinery.SourceFileLoader("public_focus_policy", str(file))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        module = importlib.util.module_from_spec(spec)
        loader.exec_module(module)
        return module

    def test_focus_receipt_discards_private_names_roles_and_hidden_controls(self):
        module = self.focus_module()
        self.assertEqual(module.public_node("Search", "entry", True, True, False),
                         {"label": "Search", "role": "entry", "focused": True, "active": False})
        for name, role, showing in [("private password", "entry", True), ("Search", "password text", True),
                                     ("Search", "entry", False), ("private filename", "frame", True)]:
            self.assertIsNone(module.public_node(name, role, showing, True, True))
        tree = ast.parse(Path(module.__file__).read_text())
        # Default observation still has no text queries. Only the separately
        # admitted controlled-query helper may compare its fixed five characters.
        queries = {n.func.attr for fn in tree.body if isinstance(fn,ast.FunctionDef) and fn.name != "controlled_query"
                   for n in ast.walk(fn) if isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute)}
        self.assertFalse(queries & {"get_text", "get_description", "get_value", "get_editable_text", "get_action", "do_action", "grab_focus"})
        observer = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "observe")
        hidden = next(n for n in ast.walk(observer) if isinstance(n, ast.If) and ast.dump(n.test) == ast.dump(ast.parse("depth and not showing", mode="eval").body))
        self.assertIsInstance(hidden.body[0], ast.Continue)

    def test_locked_guard_rejects_before_any_accessibility_tree_query(self):
        module = self.focus_module()
        tree = ast.parse(Path(module.__file__).read_text())
        observe = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "observe")
        guard = next(n for n in observe.body if isinstance(n, ast.If)
                     and isinstance(n.test, ast.Subscript) and isinstance(n.test.slice, ast.Constant)
                     and n.test.slice.value == "locked")
        scope = {"json": json, "state": SimpleNamespace(read_text=lambda: '{"locked": true}')}
        with self.assertRaisesRegex(RuntimeError, "never queried"):
            exec(compile(ast.Module(body=[guard], type_ignores=[]), module.__file__, "exec"), scope)
        guard_index = observe.body.index(guard)
        first_tree = next(i for i,n in enumerate(observe.body) if isinstance(n, ast.Assign)
                          and isinstance(n.value, ast.Call) and isinstance(n.value.func, ast.Name)
                          and n.value.func.id == "accessibility_query"
                          and isinstance(n.value.args[0], ast.Attribute) and n.value.args[0].attr == "get_desktop")
        self.assertLess(guard_index, first_tree)
        # Guard/query ordering policy only, never a genuine reader/privacy qualification.

    def test_all_principal_queries_share_lock_guard_and_dbus_deadline(self):
        module = self.focus_module()
        tree = ast.parse(Path(module.__file__).read_text())
        query = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "accessibility_query")
        calls = []
        def remaining():
            calls.append("guard")
            raise RuntimeError("locked")
        scope = {"remaining_budget": remaining, "Atspi": SimpleNamespace(set_timeout=lambda *args: None)}
        exec(compile(ast.Module(body=[query], type_ignores=[]), module.__file__, "exec"), scope)
        with self.assertRaisesRegex(RuntimeError, "locked"):
            scope["accessibility_query"](lambda: calls.append("private query"))
        self.assertEqual(calls, ["guard"])
        for n in ast.walk(tree):
            if isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute):
                self.assertNotIn(n.func.attr, {"get_desktop", "get_child_count", "get_child_at_index", "get_process_id"})
        bus = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "bus_query")
        connection = SimpleNamespace(call_sync=lambda *args: calls.append(args[-2]) or "reply")
        scope = {"remaining_budget": lambda: 75, "Gio": SimpleNamespace(DBusCallFlags=SimpleNamespace(NONE=0))}
        exec(compile(ast.Module(body=[bus], type_ignores=[]), module.__file__, "exec"), scope)
        self.assertEqual(scope["bus_query"](connection, "bus", "path", "iface", "method", None, None), "reply")
        self.assertEqual(calls[-1], 75)
        # Source ordering/shared call budget only; synchronous GI connection setup
        # itself is not claimed to have an eight-second hard completion bound.

    def test_accessibility_peer_credentials_require_original_owner_pid_and_uid(self):
        module = self.focus_module()
        tree = ast.parse(Path(module.__file__).read_text())
        credentials = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "credentials")
        refusal = next(n for n in credentials.body if isinstance(n, ast.If))
        predicate = compile(ast.Expression(refusal.test), module.__file__, "eval")
        values = {"GetNameOwner": ":1.27", "GetConnectionUnixProcessID": 933, "GetConnectionUnixUser": 1000}
        scope = {"values": values, "owner": ":1.27", "pid": 933, "uid": 1000}
        self.assertFalse(eval(predicate, scope))
        for key, invalid in (("GetNameOwner", ":1.28"), ("GetConnectionUnixProcessID", 934), ("GetConnectionUnixUser", 0)):
            self.assertTrue(eval(predicate, {**scope, "values": {**values, key: invalid}}))

    def test_public_speech_tokens_never_retain_arbitrary_presenter_text(self):
        entries = [{"kind": "speech", "text": "Search entry private trailing text"},
                   {"kind": "speech", "text": "Take Screenshot button"},
                   {"kind": "interrupt"}, {"kind": "speech", "text": "research private"}]
        self.assertEqual(lane.public_speech_tokens(entries), ["search", "take screenshot"])
        self.assertEqual(lane.public_speech_tokens([{ "kind": "speech", "text": "private filename"}]), [])
        with self.assertRaises(RuntimeError):
            lane.public_speech_tokens(["private malformed payload"])

    def test_public_capture_rejects_triggers_outside_original_pcm_window(self):
        file = Path(__file__).resolve().parents[1] / "roost-vm-lane"
        tree = ast.parse(file.read_text())
        capture = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "public_capture")
        validator = next(n for n in ast.walk(capture) if isinstance(n, ast.Call)
                         and isinstance(n.func, ast.Name) and n.func.id == "require_public_speech_window")
        self.assertEqual([ast.unparse(arg) for arg in validator.args],
                         ["start", "end", "before_trigger", "after_trigger", "len(samples)"])
        # Retain the original four-second actual-data negatives: larger profile
        # never creates samples, padding, or a renewed start clock.
        start, end, before, after = OrcaPublicWindow().receipts()
        start["receipt"].update(started_monotonic=10.0, ready_monotonic=10.5)
        for first, last in ((10.4, 11.0), (11.0, 10.9), (13.9, 14.0), (14.1, 14.2)):
            before["guest_monotonic"] = first; after["guest_monotonic"] = last
            with self.subTest(first=first, last=last), self.assertRaises(RuntimeError):
                lane.require_public_speech_window(start, end, before, after, 96000)
        volume = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "focus_volume")
        calls = [n for n in ast.walk(volume) if isinstance(n, ast.Call)
                 and isinstance(n.func, ast.Attribute) and n.func.attr == "keys"]
        self.assertEqual([tuple(ast.literal_eval(arg) for arg in n.args) for n in calls], [("tab",)])
        # Clock/source policy only: an actual VM still must qualify the fresh recording.

    def test_navigation_actions_are_physical_and_capture_failure_always_closes(self):
        file = Path(__file__).resolve().parents[1] / "roost-vm-lane"
        tree = ast.parse(file.read_text())
        capture = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "public_capture")
        guard = next(n for n in capture.body if isinstance(n, ast.Try) and n.finalbody)
        calls = []
        class Agent:
            def run(self, action):
                calls.append(action)
                return {}
        original = RuntimeError("original public focus failure")
        def action(): raise original
        scope = {"agent": Agent(), "action": action, "primary": None, "cleanup_error": None,"stage":"trigger-before"}
        exec(compile(ast.Module(body=[guard], type_ignores=[]), str(file), "exec"), scope)
        self.assertIs(scope["primary"], original)
        self.assertIn("orca-speech-end", calls)
        for name, chord in (("open_search", ("meta_l",)), ("focus_result", ("tab",)), ("open_quick", ("meta_l", "s"))):
            helper = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == name)
            call = next(n for n in ast.walk(helper) if isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute) and n.func.attr == "keys")
            self.assertEqual(tuple(ast.literal_eval(arg) for arg in call.args), chord)
        # Actual source contracts/fault ordering only; the genuine VM must still qualify navigation.


class ControlledOverviewDiagnostic(unittest.TestCase):
    def module(self):
        path=Path(lane.__file__).parents[1]/"packaging/marlin/vm-lane/roost-vm-shell-focus"
        loader=importlib.machinery.SourceFileLoader("controlled_search_policy",str(path))
        spec=importlib.util.spec_from_loader(loader.name,loader)
        module=importlib.util.module_from_spec(spec);loader.exec_module(module)
        return module

    def test_actual_controlled_query_requires_unique_entry_and_five_characters(self):
        module=self.module();calls=[]
        class Text:
            def get_character_count(self):calls.append("count");return count
            def get_text(self,start,end):calls.append((start,end));return value
        class Entry:
            def get_text_iface(self):calls.append("interface");return Text()
        entry=Entry()
        query=lambda method,*args:method(*args)
        for entries in ([],[entry,entry]):
            self.assertIsNone(module.controlled_query(entries,query));self.assertEqual(calls,[])
        for count,value,expected in ((0,"private",False),(6,"private",False),(True,"private",False),(5.0,"private",False),(5,"disks",True),(5,"other",False)):
            calls.clear();self.assertIs(module.controlled_query([entry],query),expected)
            self.assertEqual(calls,["interface","count"]+([(0,5)] if type(count) is int and count==5 else []))

    def catalog(self,changes=None,replaced=False):
        module=self.module()
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/"receipt";uid=os.getuid()
            mapped=SimpleNamespace(st_dev=7,st_ino=9,st_uid=0,st_mode=0o100444,st_size=200,
                                   st_mtime_ns=2000000003,st_ctime_ns=4000000005)
            row={"schema":1,"pid":34,"uid":uid,"start_ticks":57,"executed_key":list(module.executable_key(mapped)),
                 "sequence":1,"sampled_monotonic_ns":100000000000,"known_query_matches":True,
                 "expected_catalog_count":1,"expected_label_matches":True,"expected_ranked_count":1,
                 "rendered_target_count":1,"search_window_visible":True,"results_container_visible":True,"controlled_source_provenance":True,"authentication_boundary":False}
            row.update(changes or {});path.write_text(json.dumps(row));path.chmod(0o600)
            old_stat=os.fstat;count=0
            def fstat(fd):
                nonlocal count
                value=old_stat(fd);count+=1
                if count==2 and replaced:
                    path.rename(path.with_name("original"));path.write_text('{}');path.chmod(0o600)
                return value
            module.pathlib=SimpleNamespace(Path=lambda name:SimpleNamespace(lstat=lambda:mapped)
                              if name=="/usr/bin/roost-shell-gtk" else path)
            module.os=SimpleNamespace(open=os.open,fdopen=os.fdopen,fstat=fstat,
                stat=lambda name:mapped,readlink=lambda name:"/usr/bin/roost-shell-gtk",
                O_RDONLY=os.O_RDONLY,O_NOFOLLOW=os.O_NOFOLLOW,O_NONBLOCK=os.O_NONBLOCK)
            module.time=SimpleNamespace(monotonic_ns=lambda:100000000001)
            return module.catalog_receipt(34,uid,57)

    def test_actual_catalog_receipt_keeps_original_fd_and_cached_source_scope(self):
        value=self.catalog();self.assertEqual(value["sample"]["expected_catalog_count"],1)
        self.assertEqual(value["sample_scope"],"original-show-results")
        self.assertEqual(len(value["fd_receipt"]["sha256"]),64)
        self.assertIs(value["authentication_boundary"],False)

    def test_actual_catalog_rejects_private_unknown_fields_changed_principal_and_expiry(self):
        for changes in ({"message":"private"},{"pid":35},{"uid":True},{"start_ticks":58},
                        {"known_query_matches":False},{"expected_catalog_count":True},
                        {"expected_ranked_count":7},{"rendered_target_count":0},
                        {"search_window_visible":1},{"results_container_visible":"private"},
                        {"expected_catalog_count":2},{"executed_key":[1]*9},
                        {"sampled_monotonic_ns":100000000002},{"sampled_monotonic_ns":1},
                        {"authentication_boundary":True},{"controlled_source_provenance":False}):
            with self.subTest(fields=list(changes)),self.assertRaises(ValueError):self.catalog(changes)
        with self.assertRaises(ValueError):self.catalog(replaced=True)

    def test_controlled_shell_replacement_or_lock_refuses_before_observer_rpc(self):
        path=Path(lane.__file__).parents[1]/"packaging/marlin/vm-lane/roost-vm-lifecycle"
        fn=next(n for n in ast.parse(path.read_text()).body if isinstance(n,ast.FunctionDef) and n.name=="shell_navigation_focus")
        calls=[]
        for locked in (True,False):
            scope={"orca_observation":lambda:{"ready":True,"state":"active"},"json":json,
                   "STATE":SimpleNamespace(read_text=lambda:json.dumps({"locked":locked})),
                   "inventory":lambda:{"processes":[{"executable":"roost-shell-gtk","uid":1000,"pid":35,"start_ticks":58}]},
                   "OWNER":SimpleNamespace(pw_uid=1000),"call":lambda *args,**kw:calls.append(args)}
            exec(compile(ast.Module(body=[fn],type_ignores=[]),str(path),"exec"),scope)
            with self.assertRaises(RuntimeError):scope["shell_navigation_focus"]("shell",34,57,controlled=True)
            self.assertEqual(calls,[])

    def test_finite_host_summary_discards_private_or_oversized_values(self):
        good={"known_query_matches":True,"Disks_result":{"present":1,"showing":1,"focused":0},"focused_role_classes":["other"]}
        self.assertEqual(lane.safe_navigation_diagnostic(good),good)
        for value in (dict(good,message="private"),dict(good,known_query_matches="private"),
                      dict(good,Disks_result={"present":True,"showing":1,"focused":0}),
                      dict(good,focused_role_classes=["private"]),dict(good,focused_role_classes=["other"]*513)):
            self.assertIsNone(lane.safe_navigation_diagnostic(value))

    def test_actual_capture_finite_stage_preserves_primary_and_owned_end(self):
        tree=ast.parse(Path(lane.__file__).read_text());fn=next(n for n in ast.walk(tree) if isinstance(n,ast.FunctionDef) and n.name=="public_capture")
        calls=[];original=RuntimeError("private focus error")
        class Agent:
            def run(self,name,*args):calls.append(name);return {}
        def action():raise original
        with tempfile.TemporaryDirectory() as out:
            scope={"agent":Agent(),"os":os,"json":json,"evidence":out,"navigation":[],
                   "safe_navigation_diagnostic":lane.safe_navigation_diagnostic}
            exec(compile(ast.Module(body=[fn],type_ignores=[]),"actual-public-capture","exec"),scope)
            with self.assertRaises(RuntimeError) as error:scope["public_capture"]("orca-overview-result","disks",action)
            self.assertIs(error.exception,original)
            self.assertEqual(calls,["orca-speech-start","orca-speech-trigger","orca-speech-end"])
            data=json.loads((Path(out)/"orca-overview-result-failure.json").read_text())
            self.assertEqual(data["primary_stage"],"action-focus");self.assertIsNone(data["cleanup_stage"])
            self.assertNotIn("private",json.dumps(data))


class OrcaLifecycleAuthority(unittest.TestCase):
    def test_presenter_setup_attempt_and_reservation_failures_use_original_cleanup(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
        setup = next(node for node in ast.walk(main) if isinstance(node, ast.Try)
                     and any(isinstance(item, ast.Assign) and isinstance(item.value, ast.Call)
                             and isinstance(item.value.func, ast.Name) and item.value.func.id == "reserve_recording"
                             for item in node.body))
        cleanups, calls = [], []
        primary = lane.subprocess.TimeoutExpired(["busctl", "hidden-token"], 15)
        def rpc(*args, **kwargs):
            calls.append(args)
            raise primary
        def cleanup(recording, token, unit_started, presenter_started):
            cleanups.append((recording["owner"], presenter_started))
            return {"unit_departed": True, "presenter_closed": True, "errors": []}
        def failed(error, receipt): raise error
        scope = {"reserve_recording": lambda: (9, {"ino": 1}), "create_evidence": lambda *args: {"ino": 2},
                 "OWNER": SimpleNamespace(pw_uid=1000, pw_gid=1000), "pcm": "pcm", "log": "log",
                 "actual": {"owner": ":1.724"}, "unit": "original-unit", "selection": {}, "token": "hidden-token",
                 "call": rpc, "cleanup_speech": cleanup, "fail_speech": failed,
                 "unit_started": False, "presenter_started": False, "recorder": None, "reservation": None,
                 "file_identity": lambda info: {"ino": 1},
                 "RECORDING_STATE": SimpleNamespace(lstat=lambda: None, unlink=lambda: None),
                 "os": SimpleNamespace(close=lambda fd: None)}
        with self.assertRaises(lane.subprocess.TimeoutExpired) as error:
            exec(compile(ast.Module(body=[setup], type_ignores=[]), str(source), "exec"), scope)
        self.assertIs(error.exception, primary)
        self.assertEqual(cleanups, [(":1.724", True)])
        self.assertEqual(calls[0][6], "SetLogFileForTesting")
        calls.clear(); cleanups.clear()
        def occupied(): raise FileExistsError()
        scope.update(reserve_recording=occupied, reservation=None, presenter_started=False)
        with self.assertRaises(FileExistsError):
            exec(compile(ast.Module(body=[setup], type_ignores=[]), str(source), "exec"), scope)
        self.assertEqual(calls, [])
        self.assertEqual(cleanups, [(":1.724", False)])
        # Actual source ordering/fault policy only; no genuine Orca RPC qualification.

    def test_speech_failure_exits_without_token_bearing_exception_traceback(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helpers = [node for node in tree.body if isinstance(node, ast.FunctionDef)
                   and node.name in {"speech_error_type", "fail_speech"}]
        scope = {"json": json, "subprocess": lane.subprocess}
        exec(compile(ast.Module(body=helpers, type_ignores=[]), str(source), "exec"), scope)
        error = lane.subprocess.CalledProcessError(1, ["busctl", "hidden-token"], stderr="private-presenter")
        with patch("sys.stdout", new_callable=io.StringIO) as out:
            with self.assertRaises(SystemExit) as exit_error:
                scope["fail_speech"](error, {"unit_departed": True, "presenter_closed": True, "errors": []})
        self.assertEqual(exit_error.exception.code, 1)
        self.assertTrue(exit_error.exception.__suppress_context__)
        output = out.getvalue()
        self.assertNotIn("hidden-token", output)
        self.assertNotIn("private-presenter", output)
        self.assertEqual(json.loads(output)["orca_speech_error"]["returncode"], 1)

    def test_recorder_collection_requires_exact_properties_and_original_departure(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "recorder_unit_departure")
        scope = {}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        observe = scope["recorder_unit_departure"]
        loaded = "LoadState=loaded\nActiveState=inactive\nMainPID=0\n"
        collected = "LoadState=not-found\nActiveState=inactive\nMainPID=0\n"
        self.assertEqual(observe(0, 0, loaded, True)["properties"]["LoadState"], "loaded")
        self.assertEqual(observe(5, 4, collected, True)["stop_returncode"], 5)
        for stop, show, output, departed in [
                (0, 0, loaded.replace("inactive", "active"), True),
                (0, 0, loaded.replace("MainPID=0", "MainPID=42"), True),
                (5, 0, loaded, True), (0, 1, loaded, True),
                (5, 1, collected, True), (5, 4, collected, False), (5, 4, collected, None),
                (0, 0, loaded, False), (0, 0, "", True),
                (0, 0, collected + "Private=secret\n", True),
                (0, 0, collected + "MainPID=0\n", True),
                (0, 0, "LoadState=not-found\n", True), (True, 0, loaded, True),
                (0, 0, loaded.replace("loaded", "failed"), True)]:
            with self.subTest(output=output, departed=departed), self.assertRaises(RuntimeError):
                observe(stop, show, output, departed)
        # Synthetic systemd property policy only; actual installed VM output remains mandatory.

    def test_recorder_departure_distinguishes_original_from_recycled_pid(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "original_recorder_departed")
        ticks = 123
        def text(): return "123 (pw-record) " + " ".join(["0"] * 19 + [str(ticks)])
        scope = {}
        class Proc:
            def __truediv__(self, name): return SimpleNamespace(read_text=text)
        scope["pathlib"] = SimpleNamespace(Path=lambda path: Proc())
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        original = {"pid": 123, "start_ticks": 123, "uid": 1000}
        self.assertFalse(scope["original_recorder_departed"](original))
        ticks = 456
        self.assertTrue(scope["original_recorder_departed"](original))
        def vanished(): raise FileNotFoundError()
        text = vanished
        self.assertTrue(scope["original_recorder_departed"](original))
        # Kernel observation policy only, not a genuine recorder lifetime receipt.

    def test_speech_failure_schema_rejects_private_or_unbounded_fields(self):
        source = Path(__file__).resolve().parent / "vm_guest_agent.py"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "speech_failure_diagnostic")
        scope = {"json": json, "base64": base64, "binascii": __import__("binascii")}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        decode = scope["speech_failure_diagnostic"]
        valid = {"exception_type": "RuntimeError", "cleanup": {"unit_departed": True,
                 "presenter_closed": False, "errors": [{"stage": "owned_presenter_close",
                 "exception_type": "CalledProcessError", "returncode": 1}]}}
        def encode(value): return base64.b64encode(json.dumps({"orca_speech_error": value}).encode()).decode()
        self.assertEqual(decode(encode(valid)), valid)
        self.assertEqual(decode(encode({**valid, "returncode": 1}))["returncode"], 1)
        for code in (True, 256, "private"):
            self.assertIsNone(decode(encode({**valid, "returncode": code})))
        for bad in [None, "x" * 5465, base64.b64encode(b"\xff").decode(), "not-base64"]:
            self.assertIsNone(decode(bad))
        import copy
        for field, value in [("stage", "private-token"), ("stderr", "private-presenter-text"),
                             ("returncode", 256), ("returncode", True), ("exception_type", "private-message")]:
            invalid = copy.deepcopy(valid)
            invalid["cleanup"]["errors"][0][field] = value
            self.assertIsNone(decode(encode(invalid)))
        invalid = copy.deepcopy(valid)
        invalid["cleanup"]["errors"] *= 4
        self.assertIsNone(decode(encode(invalid)))
        self.assertIsNone(decode(encode({**valid, "argv": "private"})))

    def test_owned_speech_cleanup_retains_both_failures_without_command_data(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helpers = [node for node in tree.body if isinstance(node, ast.FunctionDef)
                   and node.name in {"speech_error_type", "cleanup_speech"}]
        calls = []
        def failed(*args, **kwargs):
            calls.append(args)
            raise lane.subprocess.CalledProcessError(1, args, stderr="private token/presenter content")
        scope = {"call": failed, "subprocess": lane.subprocess, "stop_recording_unit": lambda recording: failed("systemctl", "--user", "stop", recording["unit"])}
        exec(compile(ast.Module(body=helpers, type_ignores=[]), str(source), "exec"), scope)
        result = scope["cleanup_speech"]({"unit": "original-owned-unit", "owner": ":1.724"}, "hidden-token")
        self.assertEqual([item["stage"] for item in result["errors"]], ["owned_unit_stop", "owned_presenter_close"])
        self.assertFalse(result["unit_departed"])
        self.assertFalse(result["presenter_closed"])
        self.assertEqual(calls[0][3], "original-owned-unit")
        self.assertEqual(calls[1][3], ":1.724")
        self.assertNotIn("private", json.dumps(result))
        self.assertNotIn("hidden-token", json.dumps(result))
        # Fault injection verifies authority and error privacy; no live systemd/presenter proof.

    def test_speech_end_always_cleans_up_before_original_failure_is_raised(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
        guard = next(node for node in ast.walk(main) if isinstance(node, ast.Try)
                     and any(isinstance(item, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "cleanup"
                         for t in item.targets) for item in node.finalbody))
        cleaned = []
        primary = RuntimeError("original observation failed")
        def selection(*args): raise primary
        scope = {"speech_selection": selection, "orca_observation": lambda: {}, "recording": {}, "token": "hidden",
                 "cleanup_speech": lambda *args: cleaned.append(True) or {"errors": []}, "primary": None,
                 "pathlib": SimpleNamespace(Path=lambda *args: SimpleNamespace(read_text=lambda: "hidden")), "RUNTIME": "/run/test"}
        exec(compile(ast.Module(body=[guard], type_ignores=[]), str(source), "exec"), scope)
        self.assertIs(scope["primary"], primary)
        self.assertEqual(cleaned, [True])
        # Fault injection verifies cleanup ordering; no genuine runtime/audio qualification.

    def test_recording_read_pins_original_nofollow_file_and_bounds(self):
        import os
        import stat
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        names = {"file_identity", "create_evidence", "read_evidence"}
        helpers = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in names]
        scope = {"os": os, "stat": stat}
        exec(compile(ast.Module(body=helpers, type_ignores=[]), str(source), "exec"), scope)
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "capture"
            identity = scope["create_evidence"](path, os.getuid(), os.getgid())
            self.assertEqual(identity["mode"], 0o600)
            path.write_bytes(b"actual bounded bytes")
            self.assertEqual(scope["read_evidence"](path, identity, 64), b"actual bounded bytes")
            with self.assertRaisesRegex(RuntimeError, "bound"):
                scope["read_evidence"](path, identity, 4)
            with self.assertRaises(FileExistsError):
                scope["create_evidence"](path, os.getuid(), os.getgid())
            path.chmod(0o644)
            with self.assertRaisesRegex(RuntimeError, "identity changed"):
                scope["read_evidence"](path, identity, 64)
            path.chmod(0o600)
            original = path.with_suffix(".original")
            path.rename(original)
            path.write_bytes(b"replacement")
            with self.assertRaisesRegex(RuntimeError, "identity changed"):
                scope["read_evidence"](path, identity, 64)
            path.unlink()
            path.symlink_to(original)
            with self.assertRaises(OSError):
                scope["read_evidence"](path, identity, 64)
        # Actual filesystem authority policy; no claim of live recorder or audio qualification.

    def test_recording_graph_requires_original_sink_serial_and_directional_ports(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "require_sink_graph")
        scope = {}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        sink = {"id": 1, "type": "PipeWire:Interface:Node", "props": {"object.serial": 20}}
        synth = {"id": 2, "type": "PipeWire:Interface:Node", "props": {"object.serial": 21}}
        objects = [sink, synth,
                   {"id": 3, "type": "PipeWire:Interface:Port", "info": {"direction": "output"}, "props": {"node.id": 2}},
                   {"id": 4, "type": "PipeWire:Interface:Port", "info": {"direction": "input"}, "props": {"node.id": 1}},
                   {"id": 5, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 2, "input-node-id": 1,
                      "output-port-id": 3, "input-port-id": 4}}]
        scope["require_sink_graph"]({"objects": objects}, sink, synth)
        with self.assertRaisesRegex(RuntimeError, "duplicate"):
            scope["require_sink_graph"]({"objects": objects + [sink]}, sink, synth)
        objects[3]["info"]["direction"] = "output"
        with self.assertRaisesRegex(RuntimeError, "port/direction"):
            scope["require_sink_graph"]({"objects": objects}, sink, synth)
        objects[3]["info"]["direction"] = "input"
        objects[0] = {"id": 1, "type": "PipeWire:Interface:Node", "props": {"object.serial": 99}}
        with self.assertRaisesRegex(RuntimeError, "identity changed"):
            scope["require_sink_graph"]({"objects": objects}, sink, synth)
        # Graph schema policy only, not genuine PipeWire association evidence.

    def test_actual_51_speech_properties_require_original_owner_and_real_module(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "speech_selection")
        reader = {"ready": True, "state": "active", "pid": 7274, "start_ticks": 22069,
                  "owner": ":1.724", "uid": 1000, "bus_pid": 7274, "bus_uid": 1000,
                  "version": ["s", "51.0"], "display": ["roost-nested-872"],
                  "parent_pid": 872, "compositor_pid": 872, "bus_name": "org.gnome.Orca1.Service"}
        calls = []
        properties = {"CurrentServer": 's "Speech Dispatcher"', "CurrentSynthesizer": 's "espeak-ng"'}
        def call(*args, **kwargs):
            calls.append(args)
            return properties[args[-1]]
        scope = {"call": call, "shlex": __import__("shlex"), "orca_observation": lambda: reader,
                 "time": SimpleNamespace(monotonic=lambda: 1)}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        self.assertEqual(scope["speech_selection"](reader)["selection"]["CurrentSynthesizer"], "espeak-ng")
        self.assertTrue(all(args[3] == reader["owner"] for args in calls))
        calls.clear()
        with self.assertRaisesRegex(RuntimeError, "Orca 51"):
            scope["speech_selection"]({**reader, "version": ["s", "50.3"]})
        self.assertEqual(calls, [])
        properties["CurrentSynthesizer"] = 's "dummy"'
        with self.assertRaisesRegex(RuntimeError, "real espeak"):
            scope["speech_selection"](reader)
        properties["CurrentSynthesizer"] = 's "espeak-ng"'
        scope["orca_observation"] = lambda: {**reader, "start_ticks": 22070}
        with self.assertRaisesRegex(RuntimeError, "identity changed"):
            scope["speech_selection"](reader)
        # Property/identity policy only, not actual Orca/SSIP or audio proof.
    def test_setting_mutation_ack_never_observes_vanishing_reader(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "mutate_orca_setting")
        calls = []
        value = "false"
        def call(*args, **kwargs):
            calls.append(args)
            if args[:2] == ("gsettings", "get"):
                return value
            return ""
        def departed_reader():
            raise RuntimeError("original reader bus is already gone")
        scope = {"call": call, "orca_observation": departed_reader}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        self.assertEqual(scope["mutate_orca_setting"]("orca-off"),
                         {"requested_enabled": False, "readback_enabled": False})
        self.assertEqual(calls[0][-1], "false")
        value = "true"
        self.assertEqual(scope["mutate_orca_setting"]("orca-on")["readback_enabled"], True)
        self.assertEqual(scope["mutate_orca_setting"]("orca-reset")["requested_enabled"], None)
        value = "false"
        with self.assertRaisesRegex(RuntimeError, "readback disagrees"):
            scope["mutate_orca_setting"]("orca-on")
        value = "malformed"
        with self.assertRaisesRegex(RuntimeError, "readback disagrees"):
            scope["mutate_orca_setting"]("orca-reset")
        # The existing host await remains mandatory; this acknowledgment claims no process departure.

    def test_read_diagnostics_preserve_only_fixed_public_dbus_failure(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "orca_read_error")
        scope = {"subprocess": lane.subprocess}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        diagnose = scope["orca_read_error"]
        command = ["runuser", "-u", "roost-test", "--", "env", "PRIVATE_ENV=do-not-copy",
                   "busctl", "--user", "call", "org.freedesktop.DBus", "/org/freedesktop/DBus",
                   "org.freedesktop.DBus", "GetConnectionUnixProcessID", "s", ":1.527"]
        error = lane.subprocess.CalledProcessError(1, command, stderr="Call failed: owner vanished\n" + "x" * 1024)
        result = diagnose(error)
        self.assertEqual(result["dbus_method"], "GetConnectionUnixProcessID")
        self.assertEqual(result["returncode"], 1)
        self.assertEqual(len(result["stderr"]), 512)
        self.assertNotIn("PRIVATE_ENV", json.dumps(result))
        for unsafe in (["orca", "--testing-token=do-not-copy"],
                       [*command[:-3], "SetLogFileForTesting", "ss", "do-not-copy"]):
            result = diagnose(lane.subprocess.CalledProcessError(1, unsafe, stderr="do-not-copy"))
            self.assertNotIn("stderr", result)
            self.assertNotIn("do-not-copy", json.dumps(result))
        self.assertEqual(diagnose(FileNotFoundError(2, "do-not-copy")),
                         {"exception_type": "FileNotFoundError", "errno": 2})

    def test_guest_transport_keeps_read_failure_and_other_action_output_private(self):
        diagnostic = {"orca_read_error": {"exception_type": "CalledProcessError",
                      "returncode": 1, "dbus_method": "GetVersion", "stderr": "Call failed: owner vanished"}}
        failed = {"exited": True, "exitcode": 1,
                  "out-data": base64.b64encode(json.dumps(diagnostic).encode()).decode(),
                  "err-data": base64.b64encode(b"private arbitrary traceback").decode()}
        actor = lane.guest_agent.GuestAgent.__new__(lane.guest_agent.GuestAgent)
        for action in ("orca-read", "orca-speech-start"):
            with patch.object(actor, "command", side_effect=[{"pid": 10}, failed]):
                with self.assertRaises(RuntimeError) as caught:
                    actor.run(action)
            message = str(caught.exception)
            self.assertNotIn("private arbitrary traceback", message)
            if action == "orca-read":
                self.assertIn("owner vanished", message)
            else:
                self.assertNotIn("owner vanished", message)
        malformed = ["!", base64.b64encode(b"\xff").decode(),
                     base64.b64encode(b"[").decode(), base64.b64encode(b"[]").decode(),
                     base64.b64encode(b"x" * 4097).decode(), "A" * 5465, None, ["not text"]]
        for output in malformed:
            with patch.object(actor, "command", side_effect=[{"pid": 10}, {**failed, "out-data": output}]):
                with self.assertRaisesRegex(RuntimeError, r"guest lifecycle orca-read failed \(exit=1\)"):
                    actor.run("orca-read")
        # Transport/unit refusal evidence only; actual bus failure comes from CI.

    def test_negative_control_requires_exact_remote_denial_and_distinct_same_uid(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        result = next(node for node in ast.walk(tree) if isinstance(node, ast.Dict)
                      and any(isinstance(key, ast.Constant) and key.value == "denied" for key in node.keys))
        expression = next(value for key, value in zip(result.keys, result.values)
                          if isinstance(key, ast.Constant) and key.value == "denied")
        compiled = compile(ast.Expression(expression), str(source), "eval")
        actor = {"accepted": False, "access_denied": True,
                 "error_name": "org.freedesktop.DBus.Error.AccessDenied",
                 "caller_uid": 1000, "caller_pid": 7200}
        def qualifies(actual):
            return eval(compiled, {"actor": actual, "OWNER": SimpleNamespace(pw_uid=1000),
                                   "before": {"pid": 6874}})
        self.assertTrue(qualifies(actor))
        for invalid in ({**actor, "accepted": True}, {**actor, "access_denied": False},
                        {**actor, "error_name": "org.freedesktop.DBus.Error.NoReply"},
                        {**actor, "error_name": None}, {**actor, "caller_uid": 0},
                        {**actor, "caller_pid": 6874}):
            self.assertFalse(qualifies(invalid))
        # Parsing/refusal policy only. The VM still has to obtain the error from
        # the actual compositor, and preserve its genuine reader's authority.

    def test_native_grabs_wait_for_same_actual_reader_and_retain_samples(self):
        reader = {"pid": 6914, "start_ticks": 21798, "owner": ":1.685",
                  "uid": 1000, "bus_pid": 6914, "bus_uid": 1000,
                  "version": ["s", "50.3"], "display": ["roost-nested-840"],
                  "parent_pid": 840, "compositor_pid": 840,
                  "bus_name": "org.gnome.Orca.Service", "ready": True, "state": "active",
                  "keyboard": {"owner": ":1.685", "reader_pid": 6914, "watch": True,
                               "modifier_count": 0, "keystroke_count": 0}}
        pending = {**reader, "keyboard": dict(reader["keyboard"])}
        ready = {**reader, "keyboard": {**reader["keyboard"], "modifier_count": 2,
                                       "keystroke_count": 464}}
        observations = [reader]
        with patch.object(lane.time, "monotonic", return_value=0), patch.object(lane.time, "sleep"):
            samples = iter([pending, ready])
            self.assertIs(lane.await_orca_keyboard(reader, lambda: next(samples), observations), ready)
        self.assertEqual(observations, [reader, pending, ready])
        with patch.object(lane.time, "monotonic", return_value=0):
            with self.assertRaisesRegex(RuntimeError, "before deadline"):
                lane.await_orca_keyboard(reader, lambda: self.fail("expired wait read"), [], timeout=0)
        for replacement in ({**ready, "pid": 6915}, {**ready, "start_ticks": 21799},
                            {**ready, "owner": ":1.686"},
                            {**ready, "keyboard": {**ready["keyboard"], "owner": ":1.686"}},
                            {**ready, "keyboard": {**ready["keyboard"], "owner": None}}):
            observations = [reader]
            with patch.object(lane.time, "monotonic", return_value=0), patch.object(lane.time, "sleep"):
                with self.assertRaisesRegex(RuntimeError, "changed before"):
                    lane.await_orca_keyboard(reader, lambda: replacement, observations)
            self.assertEqual(observations, [reader, replacement])
        # Pure wait policy only: the CI VM must still supply real PID/UID/bus
        # credentials and genuine Orca WatchKeyboard/SetKeyGrabs observations.

    def test_negative_control_consumes_actual_inventory_identity_contract(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(source.read_text())
        producer = next(node for node in tree.body
                        if isinstance(node, ast.FunctionDef) and node.name == "inventory")
        record = next(node for node in ast.walk(producer) if isinstance(node, ast.Dict)
                      and any(isinstance(key, ast.Constant) and key.value == "start_ticks"
                              for key in node.keys))
        stat = ["0"] * 20
        stat[19] = "18000"
        actual = eval(compile(ast.Expression(record), str(source), "eval"), {
            "path": SimpleNamespace(name="874"), "uid": 1000,
            "executable": "roost-compositor", "stat": stat})
        consumer = next(node for node in tree.body
                        if isinstance(node, ast.FunctionDef) and node.name == "owned_compositor")
        scope = {"OWNER": SimpleNamespace(pw_uid=1000)}
        exec(compile(ast.Module(body=[consumer], type_ignores=[]), str(source), "exec"), scope)
        select = scope["owned_compositor"]
        self.assertIs(select([actual, {**actual, "executable": "roost-shell-gtk"}]), actual)
        for invalid in ([], [{**actual, "uid": 1001}], [actual, {**actual, "pid": 875}]):
            with self.assertRaises(RuntimeError):
                select(invalid)
        # This verifies producer/consumer shape and refusal policy. Only the
        # genuine VM can qualify actual bus credentials and reader exclusion.

    def test_compositor_authority_comes_from_actual_backend(self):
        source = (Path(__file__).resolve().parents[2] / "crates/compositor/src/runtime.rs").read_text()
        self.assertIn("let hardware = matches!(self.backend, Backend::Drm(_));", source)
        self.assertIn("self.orca.set_enabled(enabled && hardware);", source)
        self.assertIn("let hardware = false;", source)
        self.assertTrue('doc["screen_reader_pid"] = serde_json::json!(self.orca.pid());' in source)

    def test_no_global_orca_service_control_or_replace(self):
        root = Path(__file__).resolve().parents[2]
        compositor = (root / "crates/compositor/src/orca.rs").read_text()
        shell = (root / "crates/shell-gtk/src/orca.rs").read_text()
        self.assertNotIn('"--replace"', compositor)
        self.assertNotIn('"orca.service"', compositor)
        self.assertIn('Command::new("/usr/bin/orca")', compositor)
        self.assertIn('libc::PR_SET_PDEATHSIG, libc::SIGTERM', compositor)
        self.assertIn('libc::getppid() != parent', compositor)
        self.assertNotIn('gio::bus_get_future', shell)
        self.assertIn('control.set_screen_reader(enabled)', shell)




class BootApi(unittest.TestCase):
    def test_already_woken_held_key_prompt_does_not_submit_empty_password(self):
        tree = ast.parse((Path(__file__).resolve().parents[1] / "roost-vm-lane").read_text())
        helper = next(node for node in ast.walk(tree)
                      if isinstance(node, ast.FunctionDef) and node.name == "pam_unlock")
        events = []
        qmp = SimpleNamespace(keys=lambda *keys: events.append(keys),
                              type_text=lambda text: events.append("fixture typed"))
        scope = {"qmp": qmp, "time": SimpleNamespace(sleep=lambda delay: None),
                 "await_state": lambda predicate, timeout: predicate({"state": {"locked": False}})}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), "<actual-pam-helper>", "exec"), scope)
        scope["pam_unlock"](wake=False)
        self.assertEqual(events, [("ctrl", "a"), "fixture typed", ("ret",)])
        events.clear()
        scope["pam_unlock"]()
        self.assertEqual(events, [("ret",), ("ctrl", "a"), "fixture typed", ("ret",)])
        # Actual prompt/PAM transitions still require the genuine hardware VM.

    def test_actual_orca_commands_use_distinct_qmp_qcodes_for_chords(self):
        tree = ast.parse((Path(__file__).resolve().parents[1] / "roost-vm-lane").read_text())
        command = next(node for node in ast.walk(tree)
                       if isinstance(node, ast.FunctionDef) and node.name == "native_command")
        chords = [tuple(ast.literal_eval(arg) for arg in node.args)
                  for node in ast.walk(command) if isinstance(node, ast.Call)
                  and isinstance(node.func, ast.Attribute) and node.func.attr == "keys"]
        self.assertIn(("insert", "h"), chords)
        self.assertIn(("shift", "f4"), chords)
        for chord in chords:
            self.assertTrue(set(chord) <= {"insert", "h", "shift", "f4", "esc"})
            qmp = lane.baseline.Qmp.__new__(lane.baseline.Qmp)
            qmp.file = io.StringIO()
            qmp._read = lambda: {"return": {}}
            with patch.object(lane.baseline.time, "sleep"):
                qmp.keys(*chord)
            wire = json.loads(qmp.file.getvalue())
            self.assertEqual(wire["execute"], "send-key")
            self.assertEqual(wire["arguments"]["keys"],
                             [{"type": "qcode", "data": name} for name in chord])

    def test_qmp_command_accepts_protocol_name_argument(self):
        qmp = lane.baseline.Qmp.__new__(lane.baseline.Qmp)
        qmp.file = io.StringIO()
        qmp._read = lambda: {"return": {}}
        with tempfile.TemporaryDirectory() as out:
            lane.Recorder(qmp, out)
            qmp.cmd("trace-event-set-state", name="virtio_gpu_cmd_set_scanout", enable=True)
        wire = json.loads(qmp.file.getvalue())
        self.assertEqual(wire, {"execute": "trace-event-set-state", "arguments": {
            "name": "virtio_gpu_cmd_set_scanout", "enable": True}})

    def test_existing_four_value_performance_call_keeps_original_devices(self):
        with patch.object(lane.os.path, "exists", return_value=True), patch.object(lane.os, "access", return_value=True), patch.object(lane.subprocess, "Popen") as spawn:
            process, qmp, serial, kvm = lane.boot("disk.raw", "/out", "/scratch", 30)
            self.assertIs(process, spawn.return_value)
            self.assertEqual((qmp, serial, kvm), ("/scratch/qmp.sock", "/out/serial.log", True))
            command = spawn.call_args.args[0]
            self.assertIn("-enable-kvm", command)
            self.assertEqual(command[command.index("-m") + 1], "6144")
            self.assertEqual(command[command.index("-smp") + 1], "4")
            self.assertIn("pcie-root-port,id=roost-gpu-port,chassis=1,slot=1", command)
            self.assertIn("virtio-vga,bus=roost-gpu-port,x-pcie-pm-no-soft-reset=on,xres=1280,yres=800", command)
            shared = lane.baseline.gpu_devices()
            offset = command.index(shared[1]) - 1
            self.assertEqual(command[offset:offset + len(shared)], shared)
            self.assertNotIn("virtio-serial-pci", command)
            self.assertFalse(any("guest_agent" in value for value in command))

    def test_explicit_guest_agent_adds_transport_and_returns_socket(self):
        with patch.object(lane.os.path, "exists", return_value=True), patch.object(lane.os, "access", return_value=True), patch.object(lane.subprocess, "Popen") as spawn:
            process, qmp, serial, kvm, agent = lane.boot("disk.raw", "/out", "/scratch", 30, guest_agent=True)
            self.assertIs(process, spawn.return_value)
            self.assertEqual(agent, "/scratch/qga.sock")
            command = spawn.call_args.args[0]
            self.assertIn("virtio-serial-pci", command)
            self.assertIn("virtserialport,chardev=roost-qga,name=org.qemu.guest_agent.0", command)


class NativeMouseSelection(unittest.TestCase):
    def mouse(self, name="QEMU Virtio Mouse", absolute=False, current=False, index=8):
        return {"name":name,"absolute":absolute,"current":current,"index":index}
    def test_native_device_is_opt_in_and_preserves_performance_default(self):
        with patch.object(lane.os.path,"exists",return_value=True), patch.object(lane.os,"access",return_value=True), patch.object(lane.subprocess,"Popen") as spawn:
            lane.boot("disk.raw","/out","/scratch",30,guest_agent=True)
            self.assertNotIn("virtio-mouse-pci,id=roost-relative-mouse",spawn.call_args.args[0])
            lane.boot("disk.raw","/out","/scratch",30,guest_agent=True,native_relative_mouse=True)
            self.assertIn("virtio-mouse-pci,id=roost-relative-mouse",spawn.call_args.args[0])
    def test_actual_selected_name_index_and_mode_are_observed(self):
        class Qmp:
            def __init__(self):self.calls=[]
            def cmd(qmp,command,**arguments):
                qmp.calls.append((command,arguments))
                if command=='human-monitor-command':return ''
                return [self.mouse(current=len(qmp.calls)>1)]
        qmp=Qmp();proof=lane.select_corner_mouse(qmp,True)
        self.assertEqual(qmp.calls,[('query-mice',{}),('human-monitor-command',{'command-line':'mouse_set 8'}),('query-mice',{})])
        self.assertTrue(proof['after'][0]['current'])
        self.assertFalse(proof['requested_absolute'])
    def test_wrong_name_duplicate_mode_or_index_cannot_be_selected(self):
        bad=[[self.mouse(name='QEMU PS/2 Mouse')],[self.mouse(),self.mouse(index=9)],
             [self.mouse(absolute=True)],[self.mouse(index=True)],[self.mouse(index=-1)]]
        for inventory in bad:
            with self.subTest(inventory=inventory):
                qmp=SimpleNamespace(cmd=lambda *args,**kwargs:inventory)
                with self.assertRaises(RuntimeError):lane.select_corner_mouse(qmp,True)
    def test_failed_or_wrong_current_handler_cannot_prove_selection(self):
        for result in ([self.mouse(current=False)],[self.mouse(name='vmmouse',absolute=True,current=True)],
                       [self.mouse(current=True,index=9)]):
            replies=iter([[self.mouse()],'',result])
            with self.subTest(result=result),self.assertRaises(RuntimeError):
                lane.select_corner_mouse(SimpleNamespace(cmd=lambda *args,**kwargs:next(replies)),True)
    def test_original_handler_must_exist_and_restore_its_exact_identity(self):
        original=self.mouse(name='vmmouse',absolute=True,current=True,index=7)
        replies=iter([[original,self.mouse()],'',[original,self.mouse()]])
        proof=lane.restore_corner_mouse(SimpleNamespace(cmd=lambda *args,**kwargs:next(replies)),original)
        self.assertEqual(proof['after'][0],original)
        with self.assertRaisesRegex(RuntimeError,'disappeared'):
            lane.restore_corner_mouse(SimpleNamespace(cmd=lambda *args,**kwargs:[self.mouse()]),original)


class InputProbePlacement(unittest.TestCase):
    def test_click_uses_placement_after_first_buffer(self):
        class FrameReached(Exception):
            pass

        clicks = []
        inventory_calls = []
        provisional = {"state": {"windows": [
            {"app_id": "org.roost.VmInput", "rect": [0, 0, 800, 600]}]}}
        placed = {"state": {"windows": [
            {"app_id": "org.roost.VmInput", "rect": [500, 250, 400, 240]}]}}

        def run(action):
            if action == "inventory":
                inventory_calls.append(action)
                return provisional if len(inventory_calls) == 1 else placed
            if action == "input-ready":
                return {"pid": 42, "count": 0, "mapped": True,
                        "active": bool(clicks), "width": 400, "height": 240}
            self.assertEqual(action, "start-input-probe")
            return {"started": True}

        def frame(_path):
            raise FrameReached()

        qmp = SimpleNamespace(click=lambda x, y: clicks.append((x, y)), frame=frame)
        with tempfile.TemporaryDirectory() as out:
            with self.assertRaises(FrameReached):
                lane.run_lifecycle(qmp, SimpleNamespace(run=run), out, "/unused", lambda *_: None)
            self.assertEqual(clicks, [(600, 350)])
            mapped = json.loads((Path(out) / "lifecycle/input-mapped.json").read_text())
            self.assertEqual(mapped, placed)
            ready = json.loads((Path(out) / "lifecycle/input-ready.json").read_text())
            self.assertTrue(ready["active"])


class RepeatedCoverage(unittest.TestCase):
    def run_boots(self, root, manifests, returncodes=None):
        next_boot = iter(zip(manifests, returncodes or [0] * len(manifests)))

        def boot(command, check):
            self.assertFalse(check)
            manifest, code = next(next_boot)
            destination = Path(command[command.index("--out") + 1])
            destination.mkdir(parents=True, exist_ok=True)
            (destination / "manifest.json").write_text(json.dumps({"assertions": manifest}))
            return SimpleNamespace(returncode=code)

        args = SimpleNamespace(out=str(root), boots=5, disk="disk.raw", timeout=30,
                               tour=True, meta=[], orca_contract="distribution")
        with patch.object(lane.subprocess, "run", side_effect=boot):
            lane.repeat_boots(args)

    def test_final_lifecycle_check_does_not_claim_five_observations(self):
        startup = {"V-DRM": {"pass": True}}
        full = {**startup, "V-SUSPEND": {"pass": True}}
        with tempfile.TemporaryDirectory() as root:
            self.run_boots(root, [startup] * 4 + [full])
            proof = json.loads((Path(root) / "repeat-manifest.json").read_text())
            self.assertEqual(proof["assertion_coverage"]["V-DRM"]["passed_boots"], [1, 2, 3, 4, 5])
            self.assertEqual(proof["assertion_coverage"]["V-SUSPEND"]["observed_boots"], [5])
            lines = (Path(root) / "assertions.txt").read_text().splitlines()
            self.assertTrue(any(line.startswith("V-SUSPEND pass observed on 1/5") for line in lines))

    def test_missing_baseline_check_is_fatal_and_retained(self):
        startup = {"V-DRM": {"pass": True}}
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaisesRegex(SystemExit, "repeated boot gate failed"):
                self.run_boots(root, [startup, {}, startup, startup, startup])
            proof = json.loads((Path(root) / "repeat-manifest.json").read_text())
            self.assertEqual(proof["assertion_coverage"]["V-DRM"]["missing_boots"], [2])
            self.assertIn("V-DRM fail observed on 4/5", (Path(root) / "assertions.txt").read_text())

    def test_observed_failure_and_failed_process_each_remain_fatal(self):
        startup = {"V-DRM": {"pass": True}}
        for last, codes in [({"V-DRM": {"pass": False}}, [0] * 5),
                            (startup, [0, 0, 0, 0, 1])]:
            with self.subTest(last=last, codes=codes), tempfile.TemporaryDirectory() as root:
                with self.assertRaisesRegex(SystemExit, "repeated boot gate failed"):
                    self.run_boots(root, [startup] * 4 + [last], codes)
                proof = json.loads((Path(root) / "repeat-manifest.json").read_text())
                self.assertEqual(proof["boot_results"][-1]["returncode"], codes[-1])
                self.assertIn("V-REPEAT fail", (Path(root) / "assertions.txt").read_text())


class PciCapabilities(unittest.TestCase):
    def probe(self, pm_control, pcie=True, cycle=False):
        helper = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        tree = ast.parse(helper.read_text())
        function = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                        and node.name == "gpu_pci_caps")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            drm = root / "drm"
            pci = root / "0000:01:00.0"
            (drm / "card1").mkdir(parents=True)
            pci.mkdir()
            (drm / "card1/device").symlink_to(pci, target_is_directory=True)
            data = bytearray(256)
            data[0x34] = 0x40
            data[0x40] = 1
            data[0x41] = 0x50 if pcie else (0x40 if cycle else 0)
            data[0x44:0x46] = pm_control.to_bytes(2, "little")
            if pcie:
                data[0x50] = 0x10
                data[0x51] = 0x40 if cycle else 0
            (pci / "config").write_bytes(data)
            namespace = {"pathlib": SimpleNamespace(Path=lambda path: drm)}
            exec(compile(ast.Module(body=[function], type_ignores=[]), str(helper), "exec"), namespace)
            return namespace["gpu_pci_caps"]()["card1"]

    def test_actual_no_soft_reset_and_pcie_are_reported(self):
        caps = self.probe(8)
        self.assertTrue(caps["no_soft_reset"])
        self.assertEqual(caps["power_state"], 0)
        self.assertEqual((caps["pm_capability"], caps["pcie_capability"]), (0x40, 0x50))
        self.assertEqual(caps["slot"], "0000:01:00.0")

    def test_d3hot_is_distinct_from_no_soft_reset(self):
        caps = self.probe(3)
        self.assertFalse(caps["no_soft_reset"])
        self.assertEqual(caps["power_state"], 3)

    def test_pm_without_pcie_cannot_masquerade_as_supported_topology(self):
        self.assertEqual(self.probe(8, pcie=False)["pcie_capability"], 0)

    def test_capability_cycle_is_bounded(self):
        self.assertTrue(self.probe(8, cycle=True)["no_soft_reset"])


if __name__ == "__main__":
    unittest.main()
