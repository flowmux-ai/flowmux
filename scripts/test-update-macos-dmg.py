#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Real signed fixture DMG -> in-place app replacement, isolated from user apps."""
import json
import os
import pathlib
import plistlib
import shutil
import subprocess
import tempfile
import unittest

REPO = pathlib.Path(__file__).resolve().parents[1]


def run(*args):
    return subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=True)


def version(app):
    with (app / "Contents/Info.plist").open("rb") as source:
        return plistlib.load(source)["CFBundleShortVersionString"]


class DmgUpdateTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory(prefix="fm-dmg-", dir="/tmp")
        cls.root = pathlib.Path(cls.temp.name)
        source = cls.root / "source/Flowmux.app"
        macos = source / "Contents/MacOS"
        macos.mkdir(parents=True)
        fixture = cls.root / "sleep.c"
        fixture.write_text("#include <unistd.h>\nint main(void) { sleep(90); return 0; }\n")
        run("clang", "-arch", "arm64", str(fixture), "-o", str(cls.root / "sleep"))
        for name in ("flowmux", "flowmuxctl", "flowmux-md-viewer"):
            shutil.copyfile(cls.root / "sleep", macos / name)
            (macos / name).chmod(0o755)
        with (source / "Contents/Info.plist").open("wb") as output:
            plistlib.dump({"CFBundleIdentifier": "com.flowmux.App", "CFBundleExecutable": "flowmux",
                          "CFBundlePackageType": "APPL", "CFBundleVersion": "9.8.7",
                          "CFBundleShortVersionString": "9.8.7"}, output)
        run("codesign", "--force", "--deep", "--sign", "-", str(source))
        cls.image = cls.root / "fixture.dmg"
        run("hdiutil", "create", "-quiet", "-srcfolder", str(source.parent),
            "-format", "UDZO", str(cls.image))
        cls.source = source

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(dir=self.root)
        self.addCleanup(self.temp.cleanup)
        self.dest = pathlib.Path(self.temp.name) / "Applications with spaces/Flowmux.app"
        shutil.copytree(self.source, self.dest)
        plist = self.dest / "Contents/Info.plist"
        data = plistlib.loads(plist.read_bytes())
        data["CFBundleShortVersionString"] = "1.0.0"
        plist.write_bytes(plistlib.dumps(data))
        run("codesign", "--force", "--deep", "--sign", "-", str(self.dest))
        self.hosts = [subprocess.Popen([str(self.dest / "Contents/MacOS/flowmux"), "90"]) for _ in range(2)]
        self.addCleanup(self.cleanup_hosts)

    def cleanup_hosts(self):
        for host in self.hosts:
            if host.poll() is None:
                host.terminate()
            host.wait()

    def install(self, image=None, wanted="9.8.7", env=None):
        return subprocess.run(["/bin/sh", str(REPO / "scripts/update-macos-dmg.sh"),
                               str(image or self.image), str(self.dest), wanted],
                              text=True, capture_output=True, env=env)

    def retained(self):
        return sorted(self.dest.parent.glob(".Flowmux.app.previous.*"))

    def test_verified_dmg_replaces_the_app_while_instances_run(self):
        stale = self.dest.parent / ".Flowmux.app.pending.12345"
        stale.mkdir()
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(version(self.dest), "9.8.7")
        run("codesign", "--verify", "--deep", "--strict", str(self.dest))
        self.assertTrue(all(host.poll() is None for host in self.hosts))
        self.assertFalse(stale.exists(), "leftovers of the deferred updater are removed")
        self.assertFalse(list(self.dest.parent.glob(".Flowmux.app.pending.*")))
        retained = self.retained()
        self.assertEqual(len(retained), 1, "running instances keep their mapped files")
        self.assertEqual(version(retained[0]), "1.0.0")
        # A second update keeps the copy the hosts still map and nothing else.
        self.assertEqual(self.install().returncode, 0)
        second = self.retained()
        self.assertIn(retained[0], second, "the copy running hosts still map is kept")
        self.assertEqual(len(second), 2)
        self.cleanup_hosts()
        self.assertEqual(self.install().returncode, 0)
        self.assertNotIn(retained[0], self.retained(), "unused copies are dropped")
        self.assertEqual(len(self.retained()), 1)
        self.assertEqual(version(self.dest), "9.8.7")

    def test_native_update_controls_in_an_isolated_bundle(self):
        env = {key: value for key, value in os.environ.items() if not key.startswith("FLOWMUX_")}
        env.update(CARGO_INCREMENTAL="0", CARGO_PROFILE_DEV_DEBUG="0", CARGO_PROFILE_TEST_DEBUG="0")
        build = subprocess.run(["cargo", "test", "-p", "flowmux", "--test", "macos_native",
                                "--features", "native-smoke", "--locked", "--no-run", "--message-format=json"],
                               cwd=REPO, env=env, text=True, capture_output=True, check=True)
        artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith("{")]
        executable = next(item["executable"] for item in artifacts
                          if item.get("reason") == "compiler-artifact" and item.get("executable")
                          and item["target"]["name"] == "macos_native")
        app = pathlib.Path(self.temp.name) / "UI/Flowmux.app/Contents/MacOS/flowmux"
        app.parent.mkdir(parents=True)
        shutil.copyfile(executable, app)
        app.chmod(0o755)
        env.update(FLOWMUX_DMG_UPDATE_SMOKE_ONLY="1", GSK_RENDERER="cairo")
        command = pathlib.Path(self.temp.name) / "flowmux"
        command.symlink_to(app)
        result = subprocess.run([str(command)], env=env, text=True, capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("MACOS_DMG_UPDATE_UI_OK", result.stdout)

    def test_incomplete_or_tampered_bundle_is_never_installed(self):
        for damage in ("missing-cli", "signature"):
            with self.subTest(damage=damage):
                folder = pathlib.Path(self.temp.name) / damage
                bundle = folder / "Flowmux.app"
                shutil.copytree(self.source, bundle)
                if damage == "missing-cli":
                    (bundle / "Contents/MacOS/flowmuxctl").unlink()
                    run("codesign", "--force", "--deep", "--sign", "-", str(bundle))
                else:
                    plist = bundle / "Contents/Info.plist"
                    data = plistlib.loads(plist.read_bytes())
                    data["CFBundleName"] = "tampered"
                    plist.write_bytes(plistlib.dumps(data))
                image = pathlib.Path(self.temp.name) / (damage + ".dmg")
                run("hdiutil", "create", "-quiet", "-srcfolder", str(folder), "-format", "UDZO", str(image))
                self.assertNotEqual(self.install(image=image).returncode, 0)
                self.assertEqual(version(self.dest), "1.0.0")
                self.assertTrue(all(host.poll() is None for host in self.hosts))
                self.assertFalse(list(self.dest.parent.glob(".Flowmux.app.pending.*")))
                self.assertFalse(self.retained())
                images = plistlib.loads(subprocess.check_output(["hdiutil", "info", "-plist"]))["images"]
                self.assertFalse(any(item.get("image-path") == str(image) for item in images))

    def test_busy_readonly_image_is_detached_after_validation_failure(self):
        tools = pathlib.Path(self.temp.name) / "tools"
        tools.mkdir()
        hdiutil = tools / "hdiutil"
        hdiutil.write_text('#!/bin/sh\n'
                           'if [ "$1" = detach ] && [ "$2" != -force ]; then exit 16; fi\n'
                           'exec /usr/bin/hdiutil "$@"\n')
        hdiutil.chmod(0o755)
        env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ.get("PATH", ""))
        try:
            result = self.install(wanted="9.8.8", env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertEqual(version(self.dest), "1.0.0")
            self.assertTrue(all(host.poll() is None for host in self.hosts))
            images = plistlib.loads(subprocess.check_output(["hdiutil", "info", "-plist"]))["images"]
            self.assertFalse(any(item.get("image-path") == str(self.image) for item in images),
                             result.stdout + result.stderr)
        finally:
            # Clean only this fixture's device if the regression is present.
            images = plistlib.loads(subprocess.check_output(["hdiutil", "info", "-plist"]))["images"]
            for item in images:
                if item.get("image-path") == str(self.image):
                    device = item["system-entities"][0]["dev-entry"]
                    run("hdiutil", "detach", "-force", device)

    def test_wrong_version_and_invalid_image_preserve_running_app(self):
        self.assertNotEqual(self.install(wanted="9.8.8").returncode, 0)
        bad = pathlib.Path(self.temp.name) / "bad.dmg"
        bad.write_bytes(b"not a disk image")
        self.assertNotEqual(self.install(image=bad).returncode, 0)
        self.assertEqual(version(self.dest), "1.0.0")
        self.assertTrue(all(host.poll() is None for host in self.hosts))
        self.assertFalse(list(self.dest.parent.glob(".Flowmux.app.pending.*")))
        self.assertFalse(self.retained())


if __name__ == "__main__":
    unittest.main()
