#!/usr/bin/env python3
"""Policy and actual ZIP/tar byte tests; no compiler or image execution."""
import importlib.util
import io
import json
import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("payload", Path(__file__).with_name("roost-perf-payload.py"))
payload = importlib.util.module_from_spec(spec)
spec.loader.exec_module(payload)


class PayloadTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        # An existing genuine system ELF gives real loader/program-header bytes;
        # these controlled package fixtures do not claim to be Roost builds.
        self.binary = Path(shutil.which("true")).read_bytes()
        self.run = {"id": 12, "head_branch": "main", "event": "push", "conclusion": "success",
                    "head_sha": "a" * 40, "repository": {"id": 3}}
        self.artifact = {"id": 14, "name": "roost-arch", "expired": False, "size_in_bytes": 1,
                         "digest": None, "workflow_run": {"id": 12, "head_sha": "a" * 40,
                         "head_branch": "main", "repository_id": 3}}

    def tearDown(self):
        self.temporary.cleanup()

    def archive(self, missing=None, duplicate=None, link=None, nonelf=None, noexec=None,
                late_alias=None, leading_dot=False, directories=False):
        output = io.BytesIO()
        with tarfile.open(fileobj=output, mode="w") as archive:
            if directories:
                for name in ("./", "./usr/", "./usr/bin/"):
                    info = tarfile.TarInfo(name)
                    info.type = tarfile.DIRTYPE
                    archive.addfile(info)
            for name in (".PKGINFO",) + payload.REQUIRED:
                if name == missing:
                    continue
                data = (b"pkgname = roost\npkgver = 0.1.0-1\narch = x86_64\nsize = 100000\n"
                        if name == ".PKGINFO" else self.binary if name in payload.BINS else
                        b"[Desktop Entry]\nName=Roost\n" if name.endswith("roost.desktop") else b"receipt\n")
                if name == nonelf:
                    data = b"not ELF"
                info = tarfile.TarInfo("./" + name if leading_dot else name)
                info.size = len(data)
                info.mode = 0o644 if name == noexec or name not in payload.BINS else 0o755
                if name == link:
                    info.type, info.linkname, info.size = tarfile.SYMTYPE, "other", 0
                archive.addfile(info, io.BytesIO(data) if info.isfile() else None)
                if name == duplicate:
                    archive.addfile(info, io.BytesIO(data))
            if late_alias:
                info = tarfile.TarInfo(late_alias)
                data = b"would overwrite the already validated executable"
                info.mode, info.size = 0o755, len(data)
                archive.addfile(info, io.BytesIO(data))
        return output.getvalue()

    def inspect(self, **kwargs):
        return payload.inspect_tar(io.BytesIO(self.archive(**kwargs)))

    def test_original_run_and_artifact_metadata(self):
        self.assertEqual(payload.metadata(self.run, self.artifact, 12)["head_sha"], "a" * 40)

    def test_wrong_run_head_branch_and_repository_refused(self):
        for field, value in (("id", 13), ("head_sha", "b" * 40),
                             ("head_branch", "pull"), ("repository_id", 4)):
            artifact = json.loads(json.dumps(self.artifact))
            artifact["workflow_run"][field] = value
            with self.subTest(field=field), self.assertRaises(payload.Refusal):
                payload.metadata(self.run, artifact, 12)
        for field, value in (("head_branch", "other"), ("event", "pull_request"),
                             ("conclusion", "failure"), ("head_sha", "invalid")):
            run = dict(self.run, **{field: value})
            with self.subTest(field=field), self.assertRaises(payload.Refusal):
                payload.metadata(run, self.artifact, 12)

    def test_unexpected_api_container_types_refused_without_attribute_errors(self):
        for unexpected in (None, [], "object", 1, False):
            for run, artifact in ((unexpected, self.artifact), (self.run, unexpected),
                    (dict(self.run, repository=unexpected), self.artifact),
                    (self.run, dict(self.artifact, workflow_run=unexpected))):
                with self.subTest(unexpected=unexpected, run=run), self.assertRaises(payload.Refusal):
                    payload.metadata(run, artifact, 12)

    def test_expired_or_wrong_artifact_refused(self):
        for field, value in (("expired", True), ("name", "wrong"), ("size_in_bytes", 0),
                             ("digest", "sha256:invalid")):
            with self.subTest(field=field), self.assertRaises(payload.Refusal):
                payload.metadata(self.run, dict(self.artifact, **{field: value}), 12)

    def test_real_tar_and_elf_loader_receipt(self):
        report = self.inspect()
        self.assertEqual(set(report["required_members"]), set(payload.REQUIRED))
        self.assertEqual(report["pkginfo"]["pkgver"], "0.1.0-1")
        self.assertTrue(report["required_members"][payload.BINS[0]]["elf"]["interpreter"].startswith("/"))

    def test_missing_symlink_duplicate_nonelf_and_nonexecutable_refused(self):
        for key in ("missing", "link", "duplicate", "nonelf", "noexec"):
            with self.subTest(key=key), self.assertRaises(payload.Refusal):
                self.inspect(**{key: payload.BINS[3]})

    def test_late_tar_alias_cannot_overwrite_validated_required_binary(self):
        for alias in ("usr//bin/roost-shell-gtk", "usr/./bin/roost-shell-gtk",
                      "usr/bin/../bin/roost-shell-gtk", "././usr/bin/roost-shell-gtk",
                      "/usr/bin/roost-shell-gtk", "./usr/bin/roost-shell-gtk"):
            with self.subTest(alias=alias), self.assertRaises(payload.Refusal):
                self.inspect(late_alias=alias)

    def test_legitimate_directories_and_one_leading_dot_are_accepted(self):
        report = self.inspect(leading_dot=True, directories=True)
        self.assertEqual(set(report["required_members"]), set(payload.REQUIRED))

    def test_wrong_elf_machine_or_program_bounds_refused(self):
        for offset, replacement in ((18, b"\xb7\x00"), (54, b"\x01\x00"), (56, b"\xff\xff")):
            data = bytearray(self.binary)
            data[offset:offset + len(replacement)] = replacement
            with self.subTest(offset=offset), self.assertRaises(payload.Refusal):
                payload.elf(data)

    def zip(self, names=("roost-0.1.0-1-x86_64.pkg.tar.zst",), symlink=False):
        path = self.root / "artifact.zip"
        with zipfile.ZipFile(path, "w") as archive:
            for name in names:
                info = zipfile.ZipInfo(name)
                info.external_attr = (0o120777 if symlink else 0o100644) << 16
                archive.writestr(info, b"controlled package bytes")
        selection = payload.metadata(self.run, dict(self.artifact, size_in_bytes=path.stat().st_size), 12)
        return path, selection

    def test_real_zip_selects_one_original_regular_package(self):
        archive, selection = self.zip()
        receipt = payload.unpack(archive, self.root / "package", selection)
        self.assertEqual((self.root / "package").read_bytes(), b"controlled package bytes")
        self.assertEqual(receipt["artifact"]["size"], selection["artifact_size"])

    def test_zip_wrong_digest_size_missing_ambiguous_or_symlink_refused(self):
        archive, selection = self.zip()
        for update in ({"artifact_size": 1}, {"artifact_digest": "sha256:" + "0" * 64}):
            with self.subTest(update=update), self.assertRaises(payload.Refusal):
                payload.unpack(archive, self.root / "unused", dict(selection, **update))
        for names, link in ((("other",), False),
                            (("roost-1.pkg.tar.zst", "roost-2.pkg.tar.zst"), False),
                            (("roost-1.pkg.tar.zst",), True)):
            archive, selection = self.zip(names, link)
            with self.subTest(names=names, link=link), self.assertRaises(payload.Refusal):
                payload.unpack(archive, self.root / "unused", selection)

    def test_copy_hash_and_size_require_exact_original_bytes(self):
        path = self.root / "copy"
        path.write_bytes(b"original")
        report = {"package": payload.regular(path, 100)[1]}
        payload.copy_check(path, report)
        for replacement in (b"replaced", b"different length"):
            path.write_bytes(replacement)
            with self.assertRaises(payload.Refusal):
                payload.copy_check(path, report)

    def test_regular_reader_refuses_symlink_and_empty_file(self):
        path = self.root / "file"
        path.write_bytes(b"original")
        link = self.root / "link"
        link.symlink_to(path)
        with self.assertRaises(OSError):
            payload.regular(link, 100)
        path.write_bytes(b"")
        with self.assertRaises(payload.Refusal):
            payload.regular(path, 100)

    def test_installed_version_bytes_and_elf_checked_independently(self):
        report = self.inspect()
        for name in payload.REQUIRED:
            path = self.root / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(self.binary if name in payload.BINS else
                             b"[Desktop Entry]\nName=Roost (preview)\n" if name.endswith("roost.desktop") else b"receipt\n")
            path.chmod(0o755 if name in payload.BINS else 0o644)
        inventory = "".join("roost /" + name + "\n" for name in payload.REQUIRED)
        self.assertEqual(len(payload.installed(self.root, report, "roost 0.1.0-1\n", inventory)), len(payload.REQUIRED))
        for changed in (inventory.replace("roost /usr/bin/roost-shell-gtk\n", ""),
                        inventory + "roost /usr/bin/roost-shell-gtk\n"):
            with self.assertRaises(payload.Refusal):
                payload.installed(self.root, report, "roost 0.1.0-1\n", changed)
        with self.assertRaises(payload.Refusal):
            payload.installed(self.root, report, "roost 0.2.0-1\n", inventory)
        (self.root / payload.BINS[3]).write_bytes(b"changed")
        with self.assertRaises(payload.Refusal):
            payload.installed(self.root, report, "roost 0.1.0-1\n", inventory)

    def test_all_six_runtime_versions_include_gtk_and_reject_mismatch(self):
        report = self.inspect()
        for name in payload.BINS:
            binary = name.rsplit("/", 1)[1]
            (self.root / (binary + ".version")).write_text(binary + " 0.1.0\n")
            (self.root / (binary + ".status")).write_text("0\n")
        self.assertEqual(len(payload.runtime(self.root, report)), 6)
        (self.root / "roost-shell-gtk.status").write_text("127\n")
        with self.assertRaises(payload.Refusal):
            payload.runtime(self.root, report)
        (self.root / "roost-shell-gtk.status").write_text("0\n")
        (self.root / "roost-shell-gtk.version").write_text("roost-shell-gtk 0.2.0\n")
        with self.assertRaises(payload.Refusal):
            payload.runtime(self.root, report)

    def test_actual_container_hash_guard_refuses_before_mutation_in_and_context(self):
        container = Path(__file__).resolve().parents[2] / "packaging/marlin/Containerfile"
        guard = container.read_text().split("RUN set -eux; \\\n", 1)[1].split(
            '    && case "$PERF_PINNED_BASELINE"', 1)[0].replace("\\\n", " ")
        package = self.root / "package"
        package.write_bytes(b"original")
        guard = guard.replace("/tmp/roost.pkg.tar.zst", str(package))
        digest = hashlib.sha256(package.read_bytes()).hexdigest()
        for supplied, pinned, allowed in ((digest, "true", True), ("0" * 64, "true", False),
                ("", "true", False), ("z" * 64, "true", False), ("a", "true", False),
                ("", "false", True), (None, "false", True), (None, "true", False)):
            marker = self.root / "mutation"
            marker.unlink(missing_ok=True)
            env = dict(os.environ, PERF_PINNED_BASELINE=pinned)
            if supplied is None:
                env.pop("ROOST_PKG_SHA256", None)
            else:
                env["ROOST_PKG_SHA256"] = supplied
            result = subprocess.run(["sh", "-c", 'set -eu; ' + guard + ' && touch "' + str(marker) + '"'],
                                    env=env, capture_output=True)
            with self.subTest(supplied=supplied, pinned=pinned):
                self.assertEqual(result.returncode == 0, allowed)
                self.assertEqual(marker.exists(), allowed)

    @unittest.skipUnless(shutil.which("zstd"), "zstd must be installed by CI preflight")
    def test_cli_failure_retains_original_zip_and_unpacked_package_identity(self):
        compressed = subprocess.run(["zstd", "-c"], input=self.archive(missing=payload.BINS[3]),
                                    check=True, capture_output=True).stdout
        archive = self.root / "actual.zip"
        with zipfile.ZipFile(archive, "w") as output:
            output.writestr("roost-1.pkg.tar.zst", compressed)
        selection = payload.metadata(self.run, dict(self.artifact, size_in_bytes=archive.stat().st_size), 12)
        report = self.root / "selection.json"
        report.write_text(json.dumps(selection))
        out = self.root / "failure.json"
        result = subprocess.run(["python3", str(Path(payload.__file__)), "archive", "--zip", str(archive),
            "--package", str(self.root / "original.zst"), "--report", str(report), "--out", str(out)],
            capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        receipt = json.loads(out.read_text())
        self.assertFalse(receipt["qualified"])
        self.assertEqual(receipt["package"]["sha256"], hashlib.sha256(compressed).hexdigest())
        self.assertEqual(receipt["artifact"]["sha256"], hashlib.sha256(archive.read_bytes()).hexdigest())

    @unittest.skipUnless(shutil.which("zstd"), "zstd must be installed by CI preflight")
    def test_actual_compressed_tar_stream_and_invalid_compression(self):
        path = self.root / "package.zst"
        path.write_bytes(subprocess.run(["zstd", "-c"], input=self.archive(),
                                       check=True, capture_output=True).stdout)
        report = payload.inspect_package(path)
        self.assertEqual(report["package"]["size"], path.stat().st_size)
        self.assertGreater(report["decompressed_bytes"], 0)
        path.write_bytes(b"not compressed tar")
        with self.assertRaises((payload.Refusal, tarfile.TarError)):
            payload.inspect_package(path)


if __name__ == "__main__":
    unittest.main()
