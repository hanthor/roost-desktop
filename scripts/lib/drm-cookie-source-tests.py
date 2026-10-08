#!/usr/bin/env python3
"""Source-closure guards; mocked graphs do not qualify actual CI resolution."""
import copy
import importlib.machinery
import importlib.util
from pathlib import Path
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("cookie_source", str(ROOT/"scripts/roost-drm-cookie-source-check"))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)

class Closure(unittest.TestCase):
    def graph(self):
        packages = [{"id": name+version, "name": name, "version": version, "source": None,
                     "manifest_path": str(ROOT/"third-party"/name/"Cargo.toml")}
                    for name, version in module.EXPECTED.items()]
        return {"packages": packages, "resolve": {"nodes": [{"id": p["id"]} for p in packages]}}
    def test_actual_source_manifests_versions_lock_and_licenses(self):
        module.check_source(ROOT)
    def test_exact_single_path_graph_accepted(self):
        self.assertEqual(set(module.check_graph(ROOT, self.graph())), set(module.EXPECTED))
    def test_no_deps_metadata_cannot_qualify_resolution(self):
        graph=self.graph(); graph["resolve"]=None
        with self.assertRaises(ValueError): module.check_graph(ROOT, graph)
    def test_registry_duplicate_rejected_even_with_local_copy(self):
        graph=self.graph(); duplicate=copy.deepcopy(graph["packages"][0]); duplicate["source"]="registry+https://github.com/rust-lang/crates.io-index"; graph["packages"].append(duplicate)
        with self.assertRaises(ValueError): module.check_graph(ROOT, graph)
    def test_registry_replacement_rejected(self):
        graph=self.graph(); graph["packages"][0]["source"]="registry+https://github.com/rust-lang/crates.io-index"
        with self.assertRaises(ValueError): module.check_graph(ROOT, graph)
    def test_other_same_version_local_tree_rejected(self):
        graph=self.graph(); graph["packages"][1]["manifest_path"]="/tmp/foreign/drm-ffi/Cargo.toml"
        with self.assertRaises(ValueError): module.check_graph(ROOT, graph)
    def test_unresolved_or_wrong_version_package_rejected(self):
        for mutation in ("version", "resolve"):
            graph=self.graph()
            if mutation=="version": graph["packages"][1]["version"]="0.9.0"
            else: graph["resolve"]["nodes"].pop()
            with self.assertRaises(ValueError): module.check_graph(ROOT, graph)
    def test_missing_standalone_patch_is_not_hidden_by_shipping_patch(self):
        with tempfile.TemporaryDirectory() as directory:
            root=Path(directory)
            for relative in ("Cargo.toml", "third-party/smithay/Cargo.toml"):
                target=root/relative; target.parent.mkdir(parents=True, exist_ok=True); target.write_text((ROOT/relative).read_text())
            path=root/"third-party/smithay/Cargo.toml"
            path.write_text(path.read_text().replace('drm-ffi = { path = "../drm-ffi" }', 'drm-ffi = { version = "0.9.1" }'))
            with self.assertRaises(ValueError): module.check_source(root)

if __name__ == "__main__": unittest.main()
