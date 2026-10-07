#!/usr/bin/env python3
"""Protect the shared boot API used by the independent performance lane."""
import importlib.machinery
import importlib.util
import io
import ast
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


if __name__ == "__main__":
    unittest.main()
