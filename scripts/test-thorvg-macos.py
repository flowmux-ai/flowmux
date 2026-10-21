#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise the optional dependency prompt without installing host packages."""
import importlib.util
from pathlib import Path
import shutil
import unittest
from unittest.mock import patch
from types import SimpleNamespace
import re

spec = importlib.util.spec_from_file_location("installer", Path(__file__).with_name("test-install.py"))
installer = importlib.util.module_from_spec(spec)
spec.loader.exec_module(installer)
spec = importlib.util.spec_from_file_location("probe", Path(__file__).with_name("check-thorvg.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


class ProbeTests(unittest.TestCase):
    def test_first_c_api_library_must_have_loaders_even_if_a_later_one_does(self):
        with patch.object(probe, "candidates", return_value=["cpp-only", "no-loaders", "ready"]):
            with patch.object(probe, "check", side_effect=[None, False, True]) as check:
                self.assertFalse(probe.ready())
                self.assertEqual(check.call_count, 2)

    def test_checks_the_same_c_api_as_the_viewer(self):
        source = (installer.REPO / "crates/flowmux/src/ui/thorvg.rs").read_text()
        self.assertEqual(set(probe.SYMBOLS), set(re.findall(r'sym!\(b"(tvg_\w+)\\0"\)', source)))

    def test_missing_library_or_c_api_is_not_ready(self):
        with patch.object(probe.c, "CDLL", side_effect=OSError):
            self.assertFalse(probe.check("absent"))
        with patch.object(probe.c, "CDLL", return_value=SimpleNamespace()):
            self.assertFalse(probe.check("cpp-only"))

    def test_every_loader_is_required_and_resources_are_released(self):
        for failing in [None, b"svg", b"lottie", b"png"]:
            with self.subTest(failing=failing):
                calls = []
                lib = SimpleNamespace(**{name: (lambda *args: 0) for name in probe.SYMBOLS})
                lib.tvg_picture_new = lambda: 1
                lib.tvg_picture_load_data = lambda p, data, size, kind, *args: int(kind == failing)
                lib.tvg_paint_rel = lambda p: calls.append("release")
                lib.tvg_engine_term = lambda: calls.append("term")
                with patch.object(probe.c, "CDLL", return_value=lib):
                    self.assertEqual(probe.check("fixture"), failing is None)
                self.assertEqual(calls[-1], "term")
                self.assertIn("release", calls)


class ThorvgPromptTests(unittest.TestCase):
    run_installer = installer.MacosDependencyTests.run_installer
    calls = installer.MacosDependencyTests.calls
    # Reuse the controlling-terminal driver, not the source installer's tests.
    def setUp(self):
        installer.MacosDependencyTests.setUp(self)
        self.source = self.root / "thorvg-source"
        (self.source / "scripts").mkdir(parents=True)
        for name in ("install-thorvg-macos.sh", "check-thorvg.py"):
            shutil.copy2(installer.REPO / "scripts" / name, self.source / "scripts" / name)
        (self.source / "scripts/install-thorvg.sh").write_text(
            'echo "build $PREFIX" >> "$TEST_ROOT/calls"\n'
            '[ "${BUILD_FAIL:-}" != 1 ] || exit 42\n'
            'touch "$TEST_ROOT/ready"\n'
        )
        probe = self.bin / "python3"
        probe.write_text('#!/bin/sh\n[ -f "$TEST_ROOT/ready" ]\n')
        probe.chmod(0o755)
        self.env["HOME"] = str(self.root / "home")
        self.script = self.source / "scripts/install-thorvg-macos.sh"

    def test_ready(self):
        (self.root / "ready").touch()
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("[Y/n]", result.stdout)

    def test_decline(self):
        result = self.run_installer(answer=b"n\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("image", result.stdout)
        self.assertFalse((self.root / "ready").exists())

    def test_noninteractive(self):
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("unavailable", result.stderr)
        self.assertFalse((self.root / "ready").exists())

    def test_accept(self):
        result = self.run_installer(answer=b"\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"build {self.root}/home/.local", self.calls())
        self.assertTrue((self.root / "ready").exists())

    def test_failure(self):
        result = self.run_installer(answer=b"y\n", BUILD_FAIL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((self.root / "ready").exists())

    def test_missing_build_tools_are_installed_after_consent(self):
        result = self.run_installer(answer=b"y\n", PATH=f"{self.bin}:/usr/bin:/bin")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("brew install meson ninja", self.calls())
        self.assertLess(self.calls().index("brew install"), self.calls().index("build "))

    def test_build_tool_install_failure_does_not_build(self):
        result = self.run_installer(answer=b"y\n", PATH=f"{self.bin}:/usr/bin:/bin", BREW_FAIL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("build ", self.calls())


if __name__ == "__main__":
    unittest.main()
