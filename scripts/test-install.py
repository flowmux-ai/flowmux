#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check installer OS routing without building or changing host packages."""
import errno
import os
import pty
import select
import time
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


class MacosDependencyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="flowmux-deps-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        scripts = self.root / "source/scripts"
        scripts.mkdir(parents=True)
        self.script = scripts / "install-macos.sh"
        shutil.copy2(REPO / "scripts/install-macos.sh", self.script)
        (scripts / "install-thorvg-macos.sh").write_text('echo "thorvg-check" >> "$TEST_ROOT/calls"\n')
        sdk = self.root / "sdk/System/Library/Frameworks/WebKit.framework"
        sdk.mkdir(parents=True)
        for name in ["uname", "xcrun", "pkg-config", "brew", "cargo", "xcode-select",
                     "codesign", "iconutil", "launchctl", "open", "plutil", "xattr"]:
            path = self.bin / name
            path.write_text(f"#!{sys.executable}\n" + '''import os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
root = pathlib.Path(os.environ["TEST_ROOT"])
with (root / "calls").open("a") as log: log.write(name + " " + " ".join(sys.argv[1:]) + "\\n")
if name == "uname": print("Darwin")
elif name == "xcrun": print(root / ("missing-sdk" if os.environ.get("MISSING_SDK") else "sdk"))
elif name == "pkg-config": sys.exit(0 if (root / "installed").exists() else 1)
elif name == "brew":
    if os.environ.get("BREW_FAIL"): sys.exit(42)
    (root / "installed").touch()
elif name == "cargo": sys.exit(23)  # Stop before any real build or app mutation.
''')
            path.chmod(0o755)
        # Include the host tool paths so configure_macos_path does not prepend them.
        self.env = dict(os.environ, TEST_ROOT=str(self.root),
                        PATH=f"{self.bin}:{Path.home() / '.cargo/bin'}:/opt/homebrew/bin:/opt/homebrew/sbin:/usr/local/bin:/usr/local/sbin:/usr/bin:/bin")

    def run_installer(self, *args, answer=None, **env):
        command = ["/bin/bash", str(self.script), *args]
        environment = dict(self.env, **env)
        if answer is None:
            return subprocess.run(command, env=environment, text=True, capture_output=True,
                                  start_new_session=True, timeout=10)
        pid, terminal = pty.fork()
        if pid == 0:
            os.execve(command[0], command, environment)
        output = b""
        deadline = time.monotonic() + 10
        try:
            while time.monotonic() < deadline:
                if not select.select([terminal], [], [], 0.1)[0]:
                    continue
                try:
                    data = os.read(terminal, 8192)
                except OSError as error:
                    if error.errno == errno.EIO:
                        break
                    raise
                if not data:
                    break
                output += data
                if output.endswith(b"[Y/n] "):
                    os.write(terminal, answer)
            else:
                os.kill(pid, 9)
                self.fail(f"installer timed out: {output!r}")
        finally:
            os.close(terminal)
            _, status = os.waitpid(pid, 0)
        return subprocess.CompletedProcess(command, os.waitstatus_to_exitcode(status),
                                           output.decode(), output.decode())

    def calls(self):
        return (self.root / "calls").read_text()

    def test_check_reports_missing_without_installing(self):
        result = self.run_installer("--check")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing pkg-config modules", result.stderr)
        self.assertNotIn("brew install", self.calls())

    def test_enter_installs_missing_libraries_before_build(self):
        result = self.run_installer(answer=b"\n")
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertIn("brew install gtk4 libadwaita vte3", self.calls())
        self.assertLess(self.calls().index("brew install"), self.calls().index("cargo build"))

    def test_ready_dependencies_do_not_prompt(self):
        (self.root / "installed").touch()
        result = self.run_installer()
        self.assertEqual(result.returncode, 23, result.stderr)
        self.assertNotIn("brew install", self.calls())
        self.assertIn("thorvg-check", self.calls())

    def test_missing_dependencies_require_confirmation(self):
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("needs a terminal", result.stderr)
        self.assertNotIn("brew install", self.calls())
        self.assertNotIn("cargo build", self.calls())

    def test_decline_stops_before_install_or_build(self):
        result = self.run_installer(answer=b"n\n")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("installation cancelled", result.stdout)
        self.assertNotIn("brew install", self.calls())
        self.assertNotIn("cargo build", self.calls())

    def test_failed_package_install_stops_before_build(self):
        result = self.run_installer(answer=b"y\n", BREW_FAIL="1")
        self.assertEqual(result.returncode, 42)
        self.assertNotIn("cargo build", self.calls())

    def test_missing_sdk_offers_apple_installer(self):
        result = self.run_installer(answer=b"y\n", MISSING_SDK="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("xcode-select --install", self.calls())
        self.assertIn("rerun ./install.sh", result.stderr)
        self.assertNotIn("cargo build", self.calls())


if __name__ == "__main__":
    unittest.main()
