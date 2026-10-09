#!/usr/bin/env python3
"""Protect the shared boot API used by the independent performance lane."""
import importlib.machinery
import importlib.util
import io
import ast
import base64
import json
import os
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from types import SimpleNamespace

loader = importlib.machinery.SourceFileLoader("vm_boot_lane", str(Path(__file__).resolve().parents[1] / "tuna-vm-lane"))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


def guest_identity(prefix="tuna"):
    """The lifecycle helper's shipped-identity globals for a guest prefix."""
    compositor = f"{prefix}-compositor"
    return {"PREFIX": prefix, "COMPOSITOR": compositor, "COMPOSITOR_COMM": compositor[:15],
            "SHELL_GTK": f"{prefix}-shell-gtk", "NESTED": f"{prefix}-nested-",
            "LOCK_PAM": f"/etc/pam.d/{prefix}-lock"}


class OrcaUnreadyDiagnostic(unittest.TestCase):
    def run_probe(self, *, locked=False, changed=False, executable="/usr/bin/python3.14",
                  uid=1000, package="python", oversized=False, replaced=False):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
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
                b"WAYLAND_DISPLAY=tuna-nested-87\0DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus\0SECRET=private-never-published\0")
            state_path = root / "state"
            state_path.write_text(json.dumps({"locked": changed, "screen_reader_pid": 57}))
            executable_reads = iter([57, 58 if replaced else 57])
            scope = {"pathlib": SimpleNamespace(Path=lambda path: root / str(path).split("/")[-1]),
                     "OWNER": SimpleNamespace(pw_uid=1000), "RUNTIME": "/run/user/1000",
                     "STATE": state_path, "os": SimpleNamespace(readlink=lambda path:
                         "/usr/bin/tuna-compositor" if path.parent.name == "87" else executable,
                         stat=lambda path: SimpleNamespace(st_dev=1, st_ino=next(executable_reads), st_size=100)),
                     "json": json, "subprocess": __import__("subprocess"),
                     "call": lambda *args: package, **guest_identity()}
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
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
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
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "orca_read_error")
        scope = {"subprocess": lane.subprocess}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        diagnose = scope["orca_read_error"]
        command = ["runuser", "-u", "tuna-test", "--", "env", "PRIVATE_ENV=do-not-copy",
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
            self.assertIn("pcie-root-port,id=tuna-gpu-port,chassis=1,slot=1", command)
            self.assertIn("virtio-vga,bus=tuna-gpu-port,x-pcie-pm-no-soft-reset=on,xres=1280,yres=800", command)
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
            self.assertIn("virtserialport,chardev=tuna-qga,name=org.qemu.guest_agent.0", command)


class NativeMouseSelection(unittest.TestCase):
    def mouse(self, name="QEMU Virtio Mouse", absolute=False, current=False, index=8):
        return {"name":name,"absolute":absolute,"current":current,"index":index}
    def test_native_device_is_opt_in_and_preserves_performance_default(self):
        with patch.object(lane.os.path,"exists",return_value=True), patch.object(lane.os,"access",return_value=True), patch.object(lane.subprocess,"Popen") as spawn:
            lane.boot("disk.raw","/out","/scratch",30,guest_agent=True)
            self.assertNotIn("virtio-mouse-pci,id=tuna-relative-mouse",spawn.call_args.args[0])
            lane.boot("disk.raw","/out","/scratch",30,guest_agent=True,native_relative_mouse=True)
            self.assertIn("virtio-mouse-pci,id=tuna-relative-mouse",spawn.call_args.args[0])
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
            {"app_id": "org.tuna.VmInput", "rect": [0, 0, 800, 600]}]}}
        placed = {"state": {"windows": [
            {"app_id": "org.tuna.VmInput", "rect": [500, 250, 400, 240]}]}}

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
                               tour=True, greeter_login=False, require_kvm=False,
                               meta=[])
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
        helper = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
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


class ShippedSnapshotInventory(unittest.TestCase):
    # Run 37696030597: the graphical PAM login produced a real greetd
    # session, yet every inventory() probe failed because the published
    # tuna-desktop 0.1.0-2 snapshot predates the newer instrumentation keys.
    # Session detection must not hard-require keys the shipped binary
    # never emits.
    SHIPPED_STATE = {"locked": False, "windows": [], "focused": None,
                     "active_workspace": 0, "overview_open": True}

    def run_inventory(self, state, sessions, prefix="tuna"):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        function = next(node for node in ast.parse(source.read_text()).body
                        if isinstance(node, ast.FunctionDef) and node.name == "inventory")
        uid = os.getuid()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proc = root / "proc"
            (proc / "1234").mkdir(parents=True)
            identity = guest_identity(prefix)
            (root / identity["COMPOSITOR"]).touch()
            (proc / "1234/exe").symlink_to(root / identity["COMPOSITOR"])
            fields = ["S", "1"] + ["0"] * 17 + ["4242"]
            (proc / "1234/stat").write_text(f"1234 ({identity['COMPOSITOR_COMM']}) " + " ".join(fields))
            state_path = root / "tuna-vm-state.json"
            state_path.write_text(json.dumps(state))
            descriptions = {
                "1": "Id=1\nVTNr=1\nUser=959\nType=tty\nService=greetd\nClass=greeter",
                "3": (f"Id=3\nVTNr=1\nUser={uid}\nType=tty\n"
                      "Service=greetd\nClass=user"),
                "4": (f"Id=4\nVTNr=\nUser={uid}\nType=unspecified\n"
                      "Service=systemd-user\nClass=manager"),
            }
            def fake_call(*args, **kwargs):
                if args[:2] == ("loginctl", "list-sessions"):
                    return "".join(f"{sid} {line}\n" for sid, line in sessions)
                if args[:2] == ("loginctl", "show-session"):
                    return descriptions[args[2]]
                raise AssertionError(f"unexpected probe: {args}")
            import pathlib as real_pathlib
            scope = {"json": json, "os": os,
                     "pathlib": SimpleNamespace(
                         Path=lambda path: proc if str(path) == "/proc" else real_pathlib.Path(path)),
                     "OWNER": SimpleNamespace(pw_uid=uid, pw_name="tuna-test"),
                     "RUNTIME": f"/run/user/{uid}", "STATE": state_path,
                     "call": fake_call, **identity}
            exec(compile(ast.Module(body=[function], type_ignores=[]), str(source), "exec"), scope)
            return scope["inventory"]()

    def test_shipped_snapshot_without_newer_keys_still_detects_session(self):
        uid = os.getuid()
        result = self.run_inventory(dict(self.SHIPPED_STATE), [
            ("1", "959 greeter seat0 tty1"),
            ("3", f"{uid} tuna-test seat0 tty1"),
            ("4", f"{uid} tuna-test - -"),
        ])
        self.assertEqual(result["session"]["Id"], "3")
        self.assertEqual(result["session"]["Service"], "greetd")
        self.assertEqual(result["session_uid"], uid)
        self.assertTrue(any(p["executable"] == "tuna-compositor" for p in result["processes"]))
        self.assertTrue(result["state"]["overview_open"])
        self.assertIsNone(result["state"]["pointer_position"])
        self.assertIsNone(result["state"]["native_relative_motion_count"])

    def test_published_guest_with_pre_rename_binaries_still_detects_session(self):
        # The published flavor can still ship the package from before #505.
        uid = os.getuid()
        result = self.run_inventory(dict(self.SHIPPED_STATE), [
            ("1", "959 greeter seat0 tty1"),
            ("3", f"{uid} tuna-test seat0 tty1"),
        ], prefix="roost")  # tuna-rename: keep
        self.assertEqual(result["session"]["Id"], "3")
        self.assertEqual([p["executable"] for p in result["processes"]],
                         ["roost-compositor"])  # tuna-rename: keep

    def test_missing_owner_session_still_rejected(self):
        uid = os.getuid()
        state = {**self.SHIPPED_STATE, "pointer_position": [0, 0],
                 "native_relative_motion_count": 0}
        with self.assertRaisesRegex(RuntimeError, "expected exactly one greetd user session"):
            self.run_inventory(state, [("4", f"{uid} tuna-test - -")])


class PredatingGuestQualification(unittest.TestCase):
    # Run 37700332957: the published tuna-desktop 0.1.0-2 guest runs a healthy
    # session whose snapshot reports every newer instrumentation key as
    # None, and its shell moves no media-key volume. Newer-behavior gates
    # qualify on such guests instead of failing the whole lifecycle; the
    # current-build lane keeps proving them strictly.
    SHIPPED_STATE = {"locked": False, "windows": [], "focused": None,
                     "active_workspace": 0, "overview_open": False,
                     "pointer_position": None, "native_relative_motion_count": None,
                     "mouse_left_handed": None, "touchpad_left_handed": None,
                     "hot_corners": None}
    CURRENT_STATE = {"locked": False, "windows": [], "focused": None,
                     "active_workspace": 0, "overview_open": False,
                     "pointer_position": [0, 0], "native_relative_motion_count": 0,
                     "mouse_left_handed": False, "touchpad_left_handed": False,
                     "hot_corners": True}
    STRICT_SERIAL = ("\n".join([
        "tuna-vm-health: audio-hda ready",
        "tuna-vm-health: volume=0.40",
        "tuna-vm-health: volume=0.46",
        "tuna-vm-health: volume=0.40",
        "tuna-vm-health: volume=0.40 [MUTED]",
        "tuna-vm-health: volume=0.40",
    ]) + "\n")
    BASELINE_SERIAL = ("tuna-vm-health: audio-hda ready\n"
                       "tuna-vm-health: volume=0.40\n")

    def verdict(self, text, state=None):
        with tempfile.TemporaryDirectory() as directory:
            lifecycle = Path(directory) / "lifecycle"
            lifecycle.mkdir()
            if state is not None:
                (lifecycle / "before.json").write_text(json.dumps({"state": state}))
            return lane.media_volume_ok(text, str(lifecycle))

    def test_predating_snapshot_qualifies(self):
        self.assertTrue(lane.predates_newer_snapshot(dict(self.SHIPPED_STATE)))

    def test_current_snapshot_stays_strict(self):
        self.assertFalse(lane.predates_newer_snapshot(dict(self.CURRENT_STATE)))

    def test_partial_snapshot_stays_strict(self):
        # Fail closed: a guest with only some newer keys still takes the
        # strict path rather than silently qualifying.
        state = {**self.SHIPPED_STATE, "mouse_left_handed": False}
        self.assertFalse(lane.predates_newer_snapshot(state))

    def test_media_volume_strict_sequence_passes(self):
        ok, detail = self.verdict(self.STRICT_SERIAL, self.CURRENT_STATE)
        self.assertTrue(ok)
        self.assertNotIn("predates", detail)

    def test_media_volume_qualifies_on_predating_guest(self):
        ok, detail = self.verdict(self.BASELINE_SERIAL, self.SHIPPED_STATE)
        self.assertTrue(ok)
        self.assertIn("predates", detail)

    def test_media_volume_stays_strict_on_current_guest(self):
        ok, _ = self.verdict(self.BASELINE_SERIAL, self.CURRENT_STATE)
        self.assertFalse(ok)

    def test_media_volume_stays_strict_without_predation_evidence(self):
        ok, _ = self.verdict(self.BASELINE_SERIAL, None)
        self.assertFalse(ok)

    def test_lock_chord_held_until_lock_owns_input_on_predating_guest(self):
        # Run 37727015469: a quick Super release on the 0.1.0-2 guest is
        # dropped before the lock surface maps, latching Super so the
        # password never reaches PAM. Hold the chord on such guests only.
        self.assertGreaterEqual(lane.lock_chord_hold_ms(True), 1000)

    def test_lock_chord_stays_quick_on_current_guest(self):
        # Current guests keep exercising the quick-release regression path.
        self.assertEqual(lane.lock_chord_hold_ms(False), 80)

    def test_media_volume_requires_hda_and_baseline(self):
        ok, _ = self.verdict("tuna-vm-health: volume=0.40\n", self.SHIPPED_STATE)
        self.assertFalse(ok)
        ok, _ = self.verdict("tuna-vm-health: audio-hda ready\n", self.SHIPPED_STATE)
        self.assertFalse(ok)


if __name__ == "__main__":
    unittest.main()
