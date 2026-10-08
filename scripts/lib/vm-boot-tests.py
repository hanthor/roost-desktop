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


class OrcaUnreadyDiagnostic(unittest.TestCase):
    def run_probe(self, *, locked=False, changed=False, executable="/usr/bin/python3.14",
                  uid=1000, package="python", oversized=False, replaced=False):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/roost-vm-lifecycle"
        fn = next(n for n in ast.parse(source.read_text()).body
                  if isinstance(n, ast.FunctionDef) and n.name == "orca_child_diagnostic")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for pid, parent, ticks in ((57, 87, 100), (87, 1, 90)):
                path = root / str(pid)
                path.mkdir()
                fields = ["S", str(parent)] + ["0"] * 17 + [str(ticks)]
                (path / "stat").write_text(f"{pid} (private process name) " + " ".join(fields))
            (root / "57/environ").write_bytes(b"x" * (128 * 1024 + 1) if oversized else
                b"WAYLAND_DISPLAY=roost-nested-87\0DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus\0SECRET=private-never-published\0")
            state_path = root / "state"
            state_path.write_text(json.dumps({"locked": changed, "screen_reader_pid": 57}))
            executable_reads = iter([57, 58 if replaced else 57])
            scope = {"pathlib": SimpleNamespace(Path=lambda path: root / str(path).split("/")[-1]),
                     "OWNER": SimpleNamespace(pw_uid=1000), "RUNTIME": "/run/user/1000",
                     "STATE": state_path, "os": SimpleNamespace(readlink=lambda path:
                         "/usr/bin/roost-compositor" if path.parent.name == "87" else executable,
                         stat=lambda path: SimpleNamespace(st_dev=1, st_ino=next(executable_reads), st_size=100)),
                     "json": json, "subprocess": __import__("subprocess"),
                     "call": lambda *args: package}
            # Keep genuine file reads; only controlled /proc path/UID/exec fixtures substitute.
            exec(compile(ast.Module(body=[fn], type_ignores=[]), str(source), "exec"), scope)
            original_stat = Path.stat
            def owned_stat(path, *args, **kwargs):
                value = original_stat(path, *args, **kwargs)
                if path.name in {"57", "87"}:
                    return SimpleNamespace(st_uid=uid if path.name == "57" else 1000)
                return value
            with patch.object(Path, "stat", owned_stat):
                result = scope["orca_child_diagnostic"]({"locked": locked}, 57)
            self.assertNotIn("private", json.dumps(result))
            return result

    def test_original_unready_child_is_observed_without_claiming_ready(self):
        result = self.run_probe()
        self.assertEqual(result["status"], "observed")
        self.assertEqual((result["pid"], result["parent_pid"], result["start_ticks"]), (57, 87, 100))
        self.assertTrue(result["display_matches_parent"])
        self.assertTrue(result["session_bus_matches"])
        self.assertNotIn("ready", result)

    def test_locked_does_not_read_process(self):
        self.assertEqual(self.run_probe(locked=True), {"status": "locked"})

    def test_lock_transition_discards_all_original_identity(self):
        self.assertEqual(self.run_probe(changed=True), {"status": "changed"})

    def test_arbitrary_executable_not_published(self):
        self.assertEqual(self.run_probe(executable="/private/secret"), {"status": "executable-refused"})

    def test_wrong_uid_refused(self):
        self.assertEqual(self.run_probe(uid=0), {"status": "principal-refused"})

    def test_wrong_package_refused(self):
        self.assertEqual(self.run_probe(package="private-package"), {"status": "package-refused"})

    def test_executed_inode_replacement_discards_identity(self):
        self.assertEqual(self.run_probe(replaced=True), {"status": "changed"})

    def test_oversized_environment_refused(self):
        self.assertEqual(self.run_probe(oversized=True), {"status": "malformed"})


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
class OrcaLifecycleAuthority(unittest.TestCase):
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
                               tour=True, meta=[])
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


class KernelCommitPolicy(unittest.TestCase):
    def sample(self, generation=3, cookie=90, count=4):
        return {"session_uid":1000,"processes":[{"executable":"roost-compositor","pid":30,"uid":1000,"start_ticks":12}],
                "state":{"native_kernel_commits":{"current_native_generation":generation,"current_layout_generation":4,"commit_api":"atomic","outputs":[{
                    "crtc":39,"completion_count":count,"rejected_cookie_count":0,"queue_error_count":0,"accepted_pending_cookie":None,
                    "last_completed":{"cookie":cookie,"crtc":39,"device":226,"commit_epoch":generation,"layout_generation":4,"sequence":100,"timestamp_clock":"monotonic","timestamp_secs":9,"timestamp_subsec_ns":10}}]}}}
    def test_original_initial_and_fresh_actual_transition_receipts(self):
        old=lane.kernel_commit_receipt(self.sample())
        new=lane.kernel_commit_receipt(self.sample(4,91,5),old,True)
        self.assertEqual(new["observation"]["current_native_generation"],4)
    def test_historical_generation_or_layout_is_not_current_authority(self):
        for field in ("commit_epoch","layout_generation"):
            value=self.sample();value["state"]["native_kernel_commits"]["outputs"][0]["last_completed"][field]-=1
            with self.assertRaises(ValueError):lane.kernel_commit_receipt(value)
    def test_recycled_pid_or_other_uid_does_not_qualify_original_compositor(self):
        old=lane.kernel_commit_receipt(self.sample())
        for field in ("pid","uid","start_ticks"):
            value=self.sample(4,91,5);value["processes"][0][field]+=1
            with self.assertRaises(ValueError):lane.kernel_commit_receipt(value,old,True)
    def test_new_epoch_needs_new_cookie_and_completion_count(self):
        old=lane.kernel_commit_receipt(self.sample())
        for generation,cookie,count in [(3,91,5),(4,90,5),(4,91,4)]:
            with self.assertRaises(ValueError):lane.kernel_commit_receipt(self.sample(generation,cookie,count),old,True)
    def test_pending_old_cookie_duplicate_crtc_and_foreign_device_rejected(self):
        import copy
        for mutation in ("pending","duplicate","device"):
            value=self.sample();outputs=value["state"]["native_kernel_commits"]["outputs"]
            if mutation=="pending":outputs[0]["accepted_pending_cookie"]=90
            elif mutation=="duplicate":outputs.append(copy.deepcopy(outputs[0]))
            else:
                old=lane.kernel_commit_receipt(value);value=self.sample(4,91,5)
                value["state"]["native_kernel_commits"]["outputs"][0]["last_completed"]["device"]=227
            with self.assertRaises(ValueError):lane.kernel_commit_receipt(value,old if mutation=="device" else None,mutation=="device")
    def test_missing_private_and_bool_payloads_rejected_without_echo(self):
        for mutation in ("private","absent","bool","timestamp"):
            value=self.sample();observation=value["state"]["native_kernel_commits"]
            if mutation=="private":observation["private"]="must-not-retain"
            elif mutation=="absent":observation["current_native_generation"]=None
            elif mutation=="bool":observation["outputs"][0]["last_completed"]["commit_epoch"]=True
            else:observation["outputs"][0]["last_completed"]["timestamp_subsec_ns"]=1_000_000_000
            with self.assertRaises(ValueError) as caught:lane.kernel_commit_receipt(value)
            self.assertNotIn("must-not-retain",str(caught.exception))
    def test_replacement_between_lifecycle_before_and_initial_is_rejected(self):
        expected=lane.kernel_compositor_principal(self.sample())
        value=self.sample();value["processes"][0]["start_ticks"]+=1
        with self.assertRaises(ValueError):lane.kernel_commit_receipt(value,expected_principal=expected)
        self.assertEqual(lane.kernel_commit_receipt(self.sample(),expected_principal=expected)["principal"],expected)
    def test_actual_nonroot_uids_are_real_positive_bounded_integers(self):
        for uid in (True,False,0,-1,1 << 32,"1000"):
            value=self.sample();value["session_uid"]=uid;value["processes"][0]["uid"]=uid
            with self.assertRaises(ValueError):lane.kernel_commit_receipt(value)
        for uid in (True,-1,1 << 32):
            value=self.sample();value["processes"][0]["uid"]=uid
            with self.assertRaises(ValueError):lane.kernel_commit_receipt(value)

    def test_sequence_wrap_is_not_mistaken_for_cookie_identity(self):
        old=lane.kernel_commit_receipt(self.sample())
        value=self.sample(4,91,5);value["state"]["native_kernel_commits"]["outputs"][0]["last_completed"]["sequence"]=0
        self.assertEqual(lane.kernel_commit_receipt(value,old,True)["observation"]["outputs"][0]["last_completed"]["sequence"],0)


if __name__ == "__main__":
    unittest.main()
