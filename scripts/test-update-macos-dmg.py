#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Real signed fixture DMG -> staged app -> deferred swap, isolated from user apps."""
import json
import os
import pathlib
import plistlib
import shutil
import subprocess
import tempfile
import time
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
        cls.helper = cls.root / "swap.sh"
        shutil.copyfile(REPO / "scripts/deferred-macos-app-swap.sh", cls.helper)

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
        self.label = f"com.flowmux.update.{self.hosts[0].pid}"
        self.addCleanup(self.cleanup_hosts)

    def cleanup_hosts(self):
        for host in self.hosts:
            if host.poll() is None:
                host.terminate()
            host.wait()
        subprocess.run(["launchctl", "remove", self.label], capture_output=True)

    def install(self, image=None, wanted="9.8.7", env=None):
        return subprocess.run(["/bin/sh", str(REPO / "scripts/update-macos-dmg.sh"),
                               str(image or self.image), str(self.dest), wanted,
                               str(self.hosts[0].pid), str(self.helper)],
                              text=True, capture_output=True, env=env)

    def test_verified_dmg_waits_for_all_running_instances(self):
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(version(self.dest), "1.0.0")
        self.assertTrue(all(host.poll() is None for host in self.hosts))
        self.assertNotEqual(self.install().returncode, 0, "do not overwrite a pending update")
        # A new window can start after the updater captured the original PIDs.
        self.hosts.append(subprocess.Popen([str(self.dest / "Contents/MacOS/flowmux")]))
        self.hosts[0].terminate()
        self.hosts[0].wait()
        time.sleep(0.4)
        self.assertEqual(version(self.dest), "1.0.0")
        self.hosts[1].terminate()
        self.hosts[1].wait()
        time.sleep(0.6)
        self.assertEqual(version(self.dest), "1.0.0", "a later instance still uses this bundle")
        self.assertIsNone(self.hosts[2].poll())
        self.hosts[2].terminate()
        self.hosts[2].wait()
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if self.dest.exists() and version(self.dest) == "9.8.7":
                break
            time.sleep(0.1)
        log = self.dest.parent / ".Flowmux.app.update.log"
        self.assertEqual(version(self.dest), "9.8.7", log.read_text() if log.exists() else "no swap log")
        run("codesign", "--verify", "--deep", "--strict", str(self.dest))
        self.assertFalse(list(self.dest.parent.glob(".Flowmux.app.pending.*")))

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

    def test_incomplete_or_tampered_bundle_is_never_staged(self):
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
                images = plistlib.loads(subprocess.check_output(["hdiutil", "info", "-plist"]))["images"]
                self.assertFalse(any(item.get("image-path") == str(image) for item in images))

    def test_failed_scheduling_cleans_staging_without_replacing_the_app(self):
        tools = pathlib.Path(self.temp.name) / "tools"
        tools.mkdir()
        launchctl = tools / "launchctl"
        launchctl.write_text("#!/bin/sh\nexit 12\n")
        launchctl.chmod(0o755)
        env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ.get("PATH", ""))
        self.assertNotEqual(self.install(env=env).returncode, 0)
        self.assertEqual(version(self.dest), "1.0.0")
        self.assertTrue(all(host.poll() is None for host in self.hosts))
        self.assertFalse(list(self.dest.parent.glob(".Flowmux.app.pending.*")))

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


if __name__ == "__main__":
    unittest.main()
