#!/usr/bin/env python3
"""Bounded provenance checks for the original trusted MAIN Arch payload.

This validates acquisition and installed bytes, not source reproducibility or
performance. Receipts remain useful when a subsequent image build rejects.
"""
import argparse
import datetime
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import struct
import subprocess
import tarfile
import threading
import zipfile

MAX_PACKAGE = 128 * 1024 * 1024
MAX_MEMBER = 64 * 1024 * 1024
MAX_TAR = 512 * 1024 * 1024
BINS = tuple(f"usr/bin/{name}" for name in (
    "roost-compositor", "roost-session", "roost-shell-host",
    "roost-shell-gtk", "roost-ibus-bridge", "roost-greeter"))
REQUIRED = BINS + ("usr/share/wayland-sessions/roost.desktop",
                  "usr/lib/systemd/user/roost-session.target", "etc/pam.d/roost-lock")


class Refusal(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise Refusal(message)


def regular(path, limit):
    """Read one original regular file through a no-follow FD, checking identity."""
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    try:
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= limit,
                "file is not bounded nonempty regular data")
        with os.fdopen(os.dup(fd), "rb") as stream:
            data = stream.read(limit + 1)
        after = os.fstat(fd)
        named = os.stat(path, follow_symlinks=False)
        identity = lambda s: (s.st_dev, s.st_ino, s.st_mode, s.st_size, s.st_mtime_ns)
        require(identity(before) == identity(after) == identity(named)
                and len(data) == before.st_size, "file changed during acquisition")
        return data, {"device": before.st_dev, "inode": before.st_ino,
                      "mode": before.st_mode, "size": before.st_size,
                      "sha256": hashlib.sha256(data).hexdigest()}
    finally:
        os.close(fd)


MAX_RUN_CANDIDATES = 20


def latest_run(response, repository):
    """Choose newest creation time from one retained, bounded API page.

    Do not silently fall back to an older package if the chosen payload later
    fails validation. This selection is provenance only, not merge qualification.
    """
    require(isinstance(repository, str)
            and re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository),
            "expected repository identity invalid")
    require(isinstance(response, dict), "workflow run API response must be an object")
    runs = response.get("workflow_runs")
    count = response.get("total_count")
    require(isinstance(runs, list) and 0 < len(runs) <= MAX_RUN_CANDIDATES
            and type(count) is int and count >= len(runs),
            "workflow run candidate page is empty, malformed or exceeds bound")
    candidates = []
    ids = set()
    for run in runs:
        require(isinstance(run, dict), "workflow run candidate must be an object")
        run_id = run.get("id")
        created = run.get("created_at")
        head = run.get("head_sha")
        repo = run.get("repository")
        head_repo = run.get("head_repository")
        require(type(run_id) is int and run_id > 0 and run_id not in ids
                and isinstance(head, str) and re.fullmatch(r"[0-9a-f]{40}", head)
                and isinstance(created, str)
                and re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z", created),
                "workflow run candidate identity or creation time invalid")
        # Reject invalid calendar/time values; canonical UTC strings then sort
        # chronologically, independently of CLI/API response ordering.
        try:
            datetime.datetime.strptime(created, "%Y-%m-%dT%H:%M:%SZ")
        except ValueError:
            raise Refusal("workflow run candidate creation time invalid") from None
        require(run.get("head_branch") == "main" and run.get("event") == "push"
                and run.get("status") == "completed" and run.get("conclusion") == "success"
                and run.get("path") == ".github/workflows/ci.yml"
                and isinstance(repo, dict) and repo.get("full_name") == repository
                and isinstance(head_repo, dict) and head_repo.get("full_name") == repository,
                "candidate is not completed successful original repository MAIN ci push")
        ids.add(run_id)
        candidates.append({"run_id": run_id, "head_sha": head, "created_at": created})
    newest = max(candidates, key=lambda candidate: (candidate["created_at"], candidate["run_id"]))
    return {**newest, "candidate_count": len(candidates), "candidates": candidates,
            "scope": "newest creation time in original bounded workflow API page"}


def metadata(run, artifact, selected_run):
    require(isinstance(run, dict) and isinstance(artifact, dict),
            "run and artifact API metadata must be objects")
    owner = artifact.get("workflow_run", {})
    repository = run.get("repository", {})
    require(isinstance(owner, dict) and isinstance(repository, dict),
            "workflow run and repository API metadata must be objects")
    require(run.get("id") == selected_run and run.get("head_branch") == "main"
            and run.get("event") == "push" and run.get("conclusion") == "success",
            "original run is not successful trusted MAIN push")
    head = run.get("head_sha", "")
    require(isinstance(head, str) and re.fullmatch(r"[0-9a-f]{40}", head),
            "original run head is missing")
    require(artifact.get("name") == "roost-arch" and artifact.get("expired") is False
            and isinstance(artifact.get("id"), int) and artifact["id"] > 0
            and isinstance(artifact.get("size_in_bytes"), int)
            and 0 < artifact["size_in_bytes"] <= MAX_PACKAGE,
            "artifact identity or lifetime is invalid")
    require(owner.get("id") == selected_run and owner.get("head_sha") == head
            and owner.get("head_branch") == "main"
            and owner.get("repository_id") == repository.get("id")
            and isinstance(owner.get("repository_id"), int),
            "artifact does not belong to original selected run/head/repository")
    digest = artifact.get("digest")
    require(digest is None or (isinstance(digest, str)
            and re.fullmatch(r"sha256:[0-9a-f]{64}", digest)), "artifact digest malformed")
    return {"run_id": selected_run, "head_sha": head, "artifact_id": artifact["id"],
            "artifact_size": artifact["size_in_bytes"], "artifact_digest": digest}


def elf(data):
    require(len(data) >= 64 and data[:7] == b"\x7fELF\x02\x01\x01",
            "required executable is not little-endian ELF64")
    fields = struct.unpack_from("<HHIQQQIHHHHHH", data, 16)
    kind, machine, version, _, offset, _, _, header, entry, count, *_ = fields
    require(kind in (2, 3) and machine == 62 and version == 1 and header == 64
            and entry == 56 and 0 < count <= 128
            and offset >= 64 and offset + count * entry <= len(data),
            "ELF executable/program header malformed")
    loaders = []
    for i in range(count):
        ptype, _, start, _, _, size, _, _ = struct.unpack_from("<IIQQQQQQ", data, offset + i * entry)
        if ptype == 3:
            require(1 < size <= 256 and start + size <= len(data), "ELF interpreter bounds invalid")
            raw = data[start:start + size]
            require(raw[-1:] == b"\0" and b"\0" not in raw[:-1], "ELF interpreter malformed")
            loader = raw[:-1].decode("ascii")
            require(loader.startswith("/") and re.fullmatch(r"/[A-Za-z0-9_./+-]+", loader)
                    and ".." not in loader.split("/"), "ELF interpreter path invalid")
            loaders.append(loader)
    require(len(loaders) == 1, "required executable must have one actual ELF interpreter")
    return {"class": 64, "machine": machine, "type": kind, "interpreter": loaders[0]}


def inspect_tar(stream):
    seen = set()
    files = {}
    pkginfo = None
    with tarfile.open(fileobj=stream, mode="r|") as archive:
        for member in archive:
            require(len(seen) < 4096, "archive member bound exceeded")
            # Libarchive can normalize aliases during pacman installation.
            # Reject them before deciding a member is unknown, or a later
            # alias could overwrite a binary whose canonical bytes passed.
            name = member.name.removeprefix("./")
            if member.isdir():
                name = name.removesuffix("/")
            require((member.isdir() and name == ".") or
                    (name and all(part not in ("", ".", "..") for part in name.split("/"))),
                    "noncanonical archive member path")
            require(name not in seen, "duplicate archive member")
            seen.add(name)
            if name not in REQUIRED and name != ".PKGINFO":
                continue
            require(member.isfile() and not member.issym() and not member.islnk()
                    and 0 < member.size <= (65536 if name == ".PKGINFO" else MAX_MEMBER),
                    "required archive member is not bounded regular data")
            extracted = archive.extractfile(member)
            require(extracted is not None, "required member cannot be read")
            data = extracted.read(MAX_MEMBER + 1)
            require(len(data) == member.size, "required member is truncated")
            if name == ".PKGINFO":
                pkginfo = data.decode("utf-8")
            else:
                require(name not in BINS or member.mode & 0o111, "required binary is not executable")
                files[name] = {"size": len(data), "sha256": hashlib.sha256(data).hexdigest(),
                               "mode": member.mode}
                if name in BINS:
                    files[name]["elf"] = elf(data)
                if name == "usr/share/wayland-sessions/roost.desktop":
                    # Existing preview branding intentionally edits this one
                    # installed member; preserve both hashes and exact rule.
                    require(data.count(b"Name=Roost\n") == 1, "preview desktop branding input differs")
                    preview = data.replace(b"Name=Roost\n", b"Name=Roost (preview)\n")
                    files[name]["preview_install"] = {
                        "rule": "Name=Roost -> Name=Roost (preview)",
                        "size": len(preview), "sha256": hashlib.sha256(preview).hexdigest()}

    require(set(files) == set(REQUIRED) and pkginfo is not None, "required package members missing")
    fields = {}
    for line in pkginfo.splitlines():
        key, sep, value = line.partition(" = ")
        if key in ("pkgname", "pkgver", "arch", "size") and sep:
            require(key not in fields, "duplicate critical package metadata")
            fields[key] = value
    require(fields.get("pkgname") == "roost" and fields.get("arch") == "x86_64"
            and re.fullmatch(r"[A-Za-z0-9.+_:~-]{1,128}", fields.get("pkgver", ""))
            and re.fullmatch(r"[1-9][0-9]{0,9}", fields.get("size", "")), "package metadata invalid")
    return {"pkginfo": fields, "required_members": files, "member_count": len(seen)}


class BoundedStream:
    def __init__(self, stream):
        self.stream, self.count = stream, 0

    def read(self, size):
        require(0 < size <= 65536, "unbounded archive read refused")
        data = self.stream.read(min(size, MAX_TAR - self.count + 1))
        self.count += len(data)
        require(self.count <= MAX_TAR, "uncompressed archive bound exceeded")
        return data


def inspect_package(path):
    data, identity = regular(path, MAX_PACKAGE)
    process = subprocess.Popen(["zstd", "-dc"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL)
    # communicate would materialize the entire decompressed package. A bounded
    # writer feeds only the original pinned bytes, while streaming tar is read.
    def feed():
        try:
            process.stdin.write(data)
            process.stdin.close()
        except (BrokenPipeError, OSError):
            pass
    writer = threading.Thread(target=feed, daemon=True)
    writer.start()
    expired = threading.Event()
    def stop():
        expired.set()
        process.kill()
    timer = threading.Timer(60, stop)
    timer.start()
    try:
        stream = BoundedStream(process.stdout)
        report = inspect_tar(stream)
        # Drain remaining original bytes too; tar's end marker is not a trim.
        while stream.read(65536):
            pass
        require(process.wait(timeout=5) == 0 and not expired.is_set(), "package decompression failed or expired")
        report.update(package=identity, decompressed_bytes=stream.count)
        return report
    finally:
        timer.cancel()
        if process.poll() is None:
            process.kill()
        process.wait(timeout=5)
        writer.join(timeout=5)
        process.stdout.close()
        process.stdin.close()


def unpack(zip_path, package_path, selection):
    data, identity = regular(zip_path, MAX_PACKAGE)
    require(len(data) == selection["artifact_size"], "downloaded artifact API size differs")
    if selection["artifact_digest"] is not None:
        require(selection["artifact_digest"] == "sha256:" + identity["sha256"], "artifact digest differs")
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        members = archive.infolist()
        require(len(members) <= 1024 and len({m.filename for m in members}) == len(members),
                "duplicate or excessive ZIP members")
        candidates = [m for m in members if re.fullmatch(r"roost-[0-9][^/]*\.pkg\.tar\.zst", m.filename)]
        require(len(candidates) == 1, "artifact must contain one original roost package")
        member = candidates[0]
        mode = member.external_attr >> 16
        require((not stat.S_IFMT(mode) or stat.S_ISREG(mode)) and 0 < member.file_size <= MAX_PACKAGE,
                "artifact package is not bounded regular data")
        payload = archive.read(member)
        require(len(payload) == member.file_size, "ZIP package truncated")
    with open(package_path, "xb") as output:
        output.write(payload)
    return {"artifact": identity, "archive_member": member.filename}


def copy_check(path, report):
    _, identity = regular(path, MAX_PACKAGE)
    require((identity["size"], identity["sha256"]) ==
            (report["package"]["size"], report["package"]["sha256"]), "copied package bytes differ")
    return identity


def installed(directory, report, pacman_version, pacman_files):
    owned = []
    for line in pacman_files.splitlines():
        package, separator, path = line.partition(" ")
        require(package == "roost" and separator and path.startswith("/")
                and len(path) <= 512, "installed package inventory malformed")
        owned.append(path.removeprefix("/"))
    require(len(owned) <= 4096 and all(owned.count(name) == 1 for name in REQUIRED),
            "installed critical member is missing or duplicated in pacman inventory")
    require(pacman_version.strip() == "roost " + report["pkginfo"]["pkgver"], "installed pacman version differs")
    receipts = {}
    for name, expected in report["required_members"].items():
        data, identity = regular(Path(directory) / name, MAX_MEMBER)
        installed_expected = expected.get("preview_install", expected)
        require(identity["size"] == installed_expected["size"] and identity["sha256"] == installed_expected["sha256"],
                "installed required member bytes differ")
        if name in BINS:
            require(identity["mode"] & 0o111 and elf(data) == expected["elf"], "installed ELF differs")
        receipts[name] = identity
    return receipts


def runtime(directory, report):
    version = report["pkginfo"]["pkgver"].rsplit("-", 1)[0].split(":", 1)[-1]
    receipts = {}
    for name in BINS:
        binary = name.rsplit("/", 1)[1]
        data, identity = regular(Path(directory) / (binary + ".version"), 4096)
        status, status_identity = regular(Path(directory) / (binary + ".status"), 32)
        require(status.strip() == b"0", "installed runtime command failed")
        words = data.decode("utf-8").strip().split()
        require(len(words) == 2 and words[0] == binary and words[1] == version,
                "original installed runtime version differs")
        receipts[binary] = {"version": identity, "exit_status": status_identity}
    return receipts


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("latest-run", "select", "archive", "copy", "installed", "runtime"))
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--runs", type=Path)
    parser.add_argument("--repository")
    parser.add_argument("--run", type=Path)
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--run-id", type=int)
    parser.add_argument("--zip", type=Path)
    parser.add_argument("--package", type=Path)
    parser.add_argument("--report", type=Path)
    parser.add_argument("--installed", type=Path)
    parser.add_argument("--runtime", type=Path)
    parser.add_argument("--pacman-version", type=Path)
    parser.add_argument("--pacman-files", type=Path)
    args = parser.parse_args()
    result = {"qualified": False, "action": args.action}
    try:
        load = lambda path: json.loads(regular(path, 1024 * 1024)[0])
        if args.action == "latest-run":
            result.update(latest_run(load(args.runs), args.repository))
        elif args.action == "select":
            result.update(metadata(load(args.run), load(args.artifact), args.run_id))
        elif args.action == "archive":
            result["artifact"] = regular(args.zip, MAX_PACKAGE)[1]
            result.update(unpack(args.zip, args.package, load(args.report)))
            result["package"] = regular(args.package, MAX_PACKAGE)[1]
            result.update(inspect_package(args.package))
        elif args.action == "copy":
            result["copy"] = copy_check(args.package, load(args.report))
        elif args.action == "runtime":
            result["runtime_versions"] = runtime(args.runtime, load(args.report))
        else:
            result["installed_members"] = installed(args.installed, load(args.report),
                                                    regular(args.pacman_version, 4096)[0].decode(),
                                                    regular(args.pacman_files, 256 * 1024)[0].decode())
        result["qualified"] = True
    except (OSError, ValueError, tarfile.TarError, zipfile.BadZipFile, subprocess.SubprocessError) as error:
        result["error"] = type(error).__name__ + ": " + str(error)
        raise SystemExit("payload provenance refused") from None
    finally:
        args.out.write_text(json.dumps(result, indent=2) + "\n")
    if args.action == "latest-run":
        print(result["run_id"])
    elif args.action == "select":
        print(result["artifact_id"])


if __name__ == "__main__":
    main()
