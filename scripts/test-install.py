#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check installer OS routing without building or changing host packages."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="flowmux-install-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.source = self.root / "source with spaces"
        (self.source / "scripts").mkdir(parents=True)
        shutil.copy2(REPO / "install.sh", self.source / "install.sh")
        (self.source / "Cargo.toml").write_text('rust-version = "1.93"\n')
        (self.source / "scripts/install-macos.sh").write_text(
            'printf "%s\\n" "$@" > "$MACOS_ARGS"\nexit 37\n'
        )
        stubs = self.root / "bin"
        stubs.mkdir()
        for name in ["uname", "dpkg", "dpkg-query", "apt-get", "rustc", "cargo"]:
            stub = stubs / name
            stub.write_text(f"#!{sys.executable}\n" + '''import os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
with open(os.environ["CALLS"], "a") as log:
    log.write(name + "\\n")
if name == "uname": print(os.environ["TEST_OS"])
elif name == "dpkg-query": print("install ok installed")
elif name in ("rustc", "cargo"): print(name + " 1.99.0")
elif name == "dpkg": sys.exit(1)  # Not an Ubuntu release below 24.04.
else: sys.exit(99)  # --check must never install packages.
''')
            stub.chmod(0o755)
        self.env = dict(os.environ, HOME=str(self.root / "home"),
                        PATH=f"{stubs}:/usr/bin:/bin",
                        MACOS_ARGS=str(self.root / "macos-args"),
                        CALLS=str(self.root / "calls"))

    def run_installer(self, system, *args):
        return subprocess.run(["/bin/bash", str(self.source / "install.sh"), *args],
                              env=dict(self.env, TEST_OS=system), cwd=self.root,
                              text=True, capture_output=True, timeout=10)

    def test_macos_forwards_arguments_and_exit_status(self):
        for args in [[], ["--check"], ["--release", "--app-dir", "/tmp/My Apps",
                                     "--bin-dir", "/tmp/My Bin", "--launch"]]:
            with self.subTest(args=args):
                result = self.run_installer("Darwin", *args)
                self.assertEqual(result.returncode, 37, result.stderr)
                self.assertEqual((self.root / "macos-args").read_text(),
                                 "\n".join(args) + "\n")
        self.assertEqual(set((self.root / "calls").read_text().splitlines()), {"uname"})

    def test_linux_keeps_prerequisite_check(self):
        result = self.run_installer("Linux", "--check", "--yes")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("prerequisites OK", result.stdout)
        calls = (self.root / "calls").read_text().splitlines()
        self.assertIn("dpkg-query", calls)
        self.assertIn("cargo", calls)
        self.assertNotIn("apt-get", calls)
        self.assertFalse((self.root / "macos-args").exists())

    def test_unsupported_os_stops_before_installation(self):
        result = self.run_installer("FreeBSD", "--check")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported", result.stderr)
        self.assertEqual((self.root / "calls").read_text().splitlines(), ["uname"])
        self.assertFalse((self.root / "macos-args").exists())


if __name__ == "__main__":
    unittest.main()
