#!/usr/bin/python3
"""Self-test for scripts/rename-to-tuna: rules, keep list and idempotence."""
import hashlib
import importlib.machinery
import importlib.util
import io
import os
import subprocess
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[1] / "rename-to-tuna"
loader = importlib.machinery.SourceFileLoader("rename_to_tuna", str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
R = importlib.util.module_from_spec(spec)
loader.exec_module(R)


class Rules(unittest.TestCase):
    CASES = [
        # Crates, Rust paths, binaries.
        ('name = "roost-compositor"', 'name = "tuna-compositor"'),
        ("use roost_shell_control::Frame;", "use tuna_shell_control::Frame;"),
        ('env!("CARGO_BIN_EXE_roost-session")', 'env!("CARGO_BIN_EXE_tuna-session")'),
        ("struct RoostPrefs;", "struct TunaPrefs;"),
        # Environment variables.
        ('std::env::var("ROOST_CONTROL_SOCKET")', 'std::env::var("TUNA_CONTROL_SOCKET")'),
        # Desktop names and trace marks.
        ('Some("Roost:GNOME")', 'Some("Tuna:GNOME")'),
        ("DesktopNames=Roost;GNOME;", "DesktopNames=Tuna;GNOME;"),
        ('mark("Roost::KMS::raw-page-flip")', 'mark("Tuna::KMS::raw-page-flip")'),
        ('vec!["Roost".to_owned(), "GNOME".to_owned()]', 'vec!["Tuna".to_owned(), "GNOME".to_owned()]'),
        # Prose, without doubling "Desktop".
        ("Name=Roost", "Name=Tuna Desktop"),
        ("Roost's own pins", "Tuna Desktop's own pins"),
        ("launch one Roost desktop session", "launch one Tuna Desktop session"),
        ("Already Tuna Desktop.", "Already Tuna Desktop."),
        ("Roost-owned prefs", "Tuna-owned prefs"),
        # IDs, namespaces, CSS, paths, log prefixes.
        ("org.roost.Shell", "org.tuna.Shell"),
        ("window.roost-lock", "window.tuna-lock"),
        ('eprintln!("roost-compositor: {err}")', 'eprintln!("tuna-compositor: {err}")'),
        ("scripts/roost-journey --help", "scripts/tuna-journey --help"),
        # Packages.
        ("pkgname=roost", "pkgname=tuna-desktop"),
        ("pkgname = roost\\n", "pkgname = tuna-desktop\\n"),
        ('fields.get("pkgname") == "roost"', 'fields.get("pkgname") == "tuna-desktop"'),
        ("Package: roost", "Package: tuna-desktop"),
        ('PKG="$OUT/roost_${VERSION}_amd64.deb"', 'PKG="$OUT/tuna-desktop_${VERSION}_amd64.deb"'),
        ("pkg=$(ls packaging/arch/roost-[0-9]*.pkg.tar.zst)", "pkg=$(ls packaging/arch/tuna-desktop-[0-9]*.pkg.tar.zst)"),
        ('re.fullmatch(r"roost-[0-9][^/]*\\.pkg\\.tar\\.zst", n)', 're.fullmatch(r"tuna-desktop-[0-9][^/]*\\.pkg\\.tar\\.zst", n)'),
        ("pacman -Ql roost > files", "pacman -Ql tuna-desktop > files"),
        ("run(['dpkg', '--verify', 'roost'])", "run(['dpkg', '--verify', 'tuna-desktop'])"),
        ("owner = {'name': 'roost', 'version': '0.1.0'}", "owner = {'name': 'tuna-desktop', 'version': '0.1.0'}"),
        ('"roost 0.1.0-1\\n"', '"tuna-desktop 0.1.0-1\\n"'),
        ('"roost " + version', '"tuna-desktop " + version'),
        ("pacman -U roost.pkg.tar.zst", "pacman -U tuna.pkg.tar.zst"),
        # Repository links: current ones move, historical runs stay.
        ("url='https://github.com/hanthor/roost-desktop'", "url='https://github.com/tuna-os/tuna-desktop'"),
        ("https://github.com/hanthor/roost-desktop/actions/runs/37189354041",
         "https://github.com/hanthor/roost-desktop/actions/runs/37189354041"),
        # Third-party identifiers and kept paths stay.
        ("cargo test --lib roost_transport_bounds", "cargo test --lib roost_transport_bounds"),
        ("See third-party/reis/ROOST-PATCH.md.", "See third-party/reis/ROOST-PATCH.md."),
        ("see docs/perf/2026-10-06-417b77a/roost/report.json", "see docs/perf/2026-10-06-417b77a/roost/report.json"),
        ("Tuna Desktop was previously named Roost.", "Tuna Desktop was previously named Roost."),
        ("Formerly **Roost**. Binaries", "Formerly **Roost**. Binaries"),
        ("# Arch package for Tuna Desktop (formerly Roost).", "# Arch package for Tuna Desktop (formerly Roost)."),
        ("sed -E 's/^Name=(Tuna Desktop|Roost)$/x/' roost.desktop", "sed -E 's/^Name=(Tuna Desktop|Roost)$/x/' tuna.desktop"),
        ('names = [n for n in (b"Tuna Desktop", b"Roost") if ok(n)]', 'names = [n for n in (b"Tuna Desktop", b"Roost") if ok(n)]'),
        ("# Old packages still say Roost; either works.", "# Old packages still say Roost; either works."),
        ('install -m0644 share/compat/pam.d/roost-lock "$STAGE/etc/pam.d/tuna-lock"',
         'install -m0644 share/compat/pam.d/roost-lock "$STAGE/etc/pam.d/tuna-lock"'),
        # Markers.
        ('const OLD: &str = "ROOST_"; // tuna-rename: keep', 'const OLD: &str = "ROOST_"; // tuna-rename: keep'),
    ]

    def test_rules(self):
        for old, new in self.CASES:
            with self.subTest(old=old):
                self.assertEqual(R.rename_content(old), new)
                self.assertEqual(R.rename_content(new), new, "second pass must be a no-op")

    def test_block_and_file_markers(self):
        block = "a roost\n# tuna-rename: keep-begin\nROOST_X\nroost-y\n# tuna-rename: keep-end\nroost-z\n"
        self.assertEqual(
            R.rename_content(block),
            "a tuna\n# tuna-rename: keep-begin\nROOST_X\nroost-y\n# tuna-rename: keep-end\ntuna-z\n",
        )
        kept = "#!/bin/sh\n# tuna-rename: keep-file\nexec roost-session\n"
        self.assertEqual(R.rename_content(kept), kept)

    def test_paths(self):
        self.assertEqual(R.rename_path("scripts/lib/roost-test-window.py"), "scripts/lib/tuna-test-window.py")
        self.assertEqual(R.rename_path("share/wayland-sessions/roost.desktop"), "share/wayland-sessions/tuna.desktop")
        for kept in ("docs/perf/2026-10-04-x/roost-report.json", "third-party/smithay/ROOST-PATCH.md",
                     "share/compat/pam.d/roost-lock", ".spektacular/x/roost.md",
                     "tests/protocols/marlin-roost-wayland-info.txt", "scripts/rename-to-tuna"):
            self.assertTrue(R.kept_path(kept), kept)
        self.assertFalse(R.kept_path("docs/perfect.md"))
        self.assertFalse(R.kept_path("docs/perf/README.md"))
        self.assertFalse(R.kept_path("docs/niri-parity.md"))


FIXTURE = {
    "Cargo.toml": '[package]\nname = "roost-demo"\n',
    "src/main.rs": 'use roost_demo::x;\nfn main() { std::env::var("ROOST_SCALE").ok(); }\n',
    "scripts/roost-journey": '#!/bin/sh\nexec target/debug/roost-compositor --title "Roost"\n',
    "scripts/lib/roost-test-window.py": "APP_ID = 'roost-test-alpha'\n",
    "share/wayland-sessions/roost.desktop": "[Desktop Entry]\nName=Roost\nExec=roost-session\nDesktopNames=Roost;GNOME;\n",
    "share/compat/wayland-sessions/roost.desktop": "[Desktop Entry]\nName=Tuna Desktop\nExec=tuna-session\n",
    "docs/perf/2026-10-06/roost/report.json": '{"desktop": "roost"}\n',
    "docs/guide.md": "Run Roost. Logs: https://github.com/hanthor/roost-desktop/actions/runs/1\n",
    "third-party/lib/ROOST-PATCH.md": "Roost patch.\n",
    "tests/a11y/gnome51/comparison.md": "Roost: 6 nodes\n",
    "packaging/compat.sh": "# tuna-rename: keep-file\nln -s tuna-session roost-session\n",
    "packaging/PKGBUILD": "pkgname=roost\nreplaces=('roost')  # tuna-rename: keep\n",
}


def tree_digest(root):
    digest = hashlib.sha256()
    for path in sorted(p for p in root.rglob("*") if ".git" not in p.relative_to(root).parts):
        digest.update(str(path.relative_to(root)).encode())
        if path.is_file():
            digest.update(path.read_bytes())
            digest.update(oct(path.stat().st_mode).encode())
    return digest.hexdigest()


class Checkout(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = Path(self.tmp.name)
        for rel, text in FIXTURE.items():
            path = self.root / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text)
        (self.root / "scripts/roost-journey").chmod(0o755)
        (self.root / "docs/roost-logo.bin").write_bytes(b"\0roost\0")
        git = ["git", "-C", str(self.root)]
        subprocess.run(git + ["init", "-q"], check=True)
        subprocess.run(git + ["add", "-A"], check=True)
        # One untracked file: branches carry new work too.
        (self.root / "scripts/lib/roost-new-probe.py").write_text("print('roost')\n")

    def tearDown(self):
        self.tmp.cleanup()

    def run_script(self, *args):
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            status = R.main(["--root", str(self.root), "--no-cargo", *args])
        return status, out.getvalue() + err.getvalue()

    def read(self, rel):
        return (self.root / rel).read_text()

    def test_check_mode_changes_nothing(self):
        before = tree_digest(self.root)
        status, output = self.run_script("--check")
        self.assertEqual(status, 1)
        self.assertIn("would move scripts/roost-journey -> scripts/tuna-journey", output)
        self.assertEqual(tree_digest(self.root), before)

    def test_rename_then_rerun_is_a_noop(self):
        kept = {rel: FIXTURE[rel] for rel in (
            "share/compat/wayland-sessions/roost.desktop",
            "docs/perf/2026-10-06/roost/report.json",
            "third-party/lib/ROOST-PATCH.md",
            "packaging/compat.sh",
        )}
        status, _ = self.run_script()
        self.assertEqual(status, 0)

        self.assertEqual(self.read("Cargo.toml"), '[package]\nname = "tuna-demo"\n')
        self.assertIn('use tuna_demo::x;', self.read("src/main.rs"))
        self.assertIn('"TUNA_SCALE"', self.read("src/main.rs"))
        self.assertEqual(self.read("scripts/tuna-journey"),
                         '#!/bin/sh\nexec target/debug/tuna-compositor --title "Tuna Desktop"\n')
        self.assertTrue(os.access(self.root / "scripts/tuna-journey", os.X_OK))
        self.assertEqual(self.read("scripts/lib/tuna-test-window.py"), "APP_ID = 'tuna-test-alpha'\n")
        self.assertEqual(self.read("scripts/lib/tuna-new-probe.py"), "print('tuna')\n")
        self.assertEqual(self.read("share/wayland-sessions/tuna.desktop"),
                         "[Desktop Entry]\nName=Tuna Desktop\nExec=tuna-session\nDesktopNames=Tuna;GNOME;\n")
        self.assertEqual(self.read("docs/guide.md"),
                         "Run Tuna Desktop. Logs: https://github.com/hanthor/roost-desktop/actions/runs/1\n")
        self.assertEqual(self.read("packaging/PKGBUILD"),
                         "pkgname=tuna-desktop\nreplaces=('roost')  # tuna-rename: keep\n")
        for rel, text in kept.items():
            self.assertEqual(self.read(rel), text, rel)
        self.assertEqual((self.root / "docs/roost-logo.bin").exists(), False)
        self.assertEqual((self.root / "docs/tuna-logo.bin").read_bytes(), b"\0roost\0")
        self.assertEqual(self.read("tests/a11y/gnome51/comparison.md"), "Tuna Desktop: 6 nodes\n")
        self.assertFalse((self.root / "scripts/roost-journey").exists())
        staged = subprocess.run(["git", "-C", str(self.root), "diff", "--cached", "--name-status"],
                                check=True, stdout=subprocess.PIPE).stdout.decode()
        self.assertIn("scripts/tuna-journey", staged, "tracked files move with git mv")

        after = tree_digest(self.root)
        self.assertEqual(self.run_script("--check")[0], 0)
        self.assertEqual(self.run_script()[0], 0)
        self.assertEqual(tree_digest(self.root), after, "a second run must change nothing")

    def test_existing_destination_is_reported_not_clobbered(self):
        (self.root / "scripts/tuna-journey").write_text("mine\n")
        status, output = self.run_script()
        self.assertEqual(status, 2)
        self.assertIn("already exists", output)
        self.assertEqual(self.read("scripts/tuna-journey"), "mine\n")


if __name__ == "__main__":
    unittest.main()
