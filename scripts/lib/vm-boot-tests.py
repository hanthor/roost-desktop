#!/usr/bin/env python3
"""Protect the shared boot API used by the independent performance lane."""
import importlib.machinery
import importlib.util
import io
import ast
import base64
import json
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from types import SimpleNamespace

loader = importlib.machinery.SourceFileLoader("vm_boot_lane", str(Path(__file__).resolve().parents[1] / "roost-vm-lane"))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


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
class OrcaLifecycleAuthority(unittest.TestCase):
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
