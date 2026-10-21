#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run the real installer against harmless Rust binaries and private destinations."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

REPO = Path(__file__).resolve().parents[1]


def run(*args, **kwargs):
    result = subprocess.run(args, text=True, capture_output=True, timeout=120, **kwargs)
    assert result.returncode == 0, result.stdout + result.stderr
    return result.stdout


def main():
    assert os.uname().sysname == "Darwin", "macOS installer test requires macOS"
    with tempfile.TemporaryDirectory(prefix="fm-install-", dir="/tmp") as temporary:
        root = Path(temporary).resolve()
        source = root / "source"
        (source / "scripts").mkdir(parents=True)
        shutil.copy2(REPO / "install.sh", source / "install.sh")
        shutil.copy2(REPO / "scripts/install-macos.sh", source / "scripts/install-macos.sh")
        for relative in ("LICENSE", "NOTICE", "docs/legal/THIRD_PARTY_LICENSES.md"):
            dest = source / relative
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(REPO / relative, dest)
        shutil.copytree(REPO / "resources/icons", source / "resources/icons")
        (source / "Cargo.toml").write_text('''[workspace]
members = ["flowmux", "flowmux-cli", "flowmux-md-viewer"]
resolver = "2"
[workspace.package]
version = "0.0.0"
[profile.fast]
inherits = "dev"
''')
        for package, binary in [("flowmux", "flowmux"), ("flowmux-cli", "flowmuxctl"), ("flowmux-md-viewer", "flowmux-md-viewer")]:
            crate = source / package
            (crate / "src").mkdir(parents=True)
            (crate / "Cargo.toml").write_text(f'''[package]
name = "{package}"
version = "0.0.0"
edition = "2021"
[[bin]]
name = "{binary}"
path = "src/main.rs"
''')
        env = {k: v for k, v in os.environ.items() if not k.startswith(("FLOWMUX_", "CARGO_TARGET", "CARGO_BUILD_TARGET"))}
        host = next(line.split(": ", 1)[1] for line in run("rustc", "-vV").splitlines() if line.startswith("host:"))
        config = source / ".cargo/config.toml"
        config.parent.mkdir()
        # Seed a valid stale Mach-O in the old hard-coded directory.
        for package in ["flowmux", "flowmux-cli", "flowmux-md-viewer"]:
            (source / package / "src/main.rs").write_text('fn main() { println!("STALE"); }\n')
        run("cargo", "build", "--profile", "fast", cwd=source, env=env)
        for package in ["flowmux", "flowmux-cli", "flowmux-md-viewer"]:
            (source / package / "src/main.rs").write_text('fn main() { println!("FRESH"); }\n')
        for name, profile, custom, configuration in [
            ("absolute", "fast", str(root / "custom target"), ""),
            ("relative", "debug", "relative-target", ""),
            ("config", "release", None, '[build]\ntarget-dir = "config-target"\n'),
            ("triple", "fast", None, f'[build]\ntarget-dir = "triple-target"\ntarget = "{host}"\n'),
            ("default", "fast", None, ""),
        ]:
            config.write_text(configuration)
            case_env = dict(env)
            if custom is not None:
                case_env["CARGO_TARGET_DIR"] = custom
            apps, bins = root / f"apps-{name}", root / f"bin-{name}"
            run("bash", str(source / "install.sh"), f"--{profile}", "--app-dir", str(apps), "--bin-dir", str(bins), cwd=root, env=case_env)
            bundle = apps / "Flowmux.app"
            run("codesign", "--verify", "--deep", str(bundle))
            for directory in [bins, bundle / "Contents/MacOS"]:
                for binary in ["flowmux", "flowmuxctl", "flowmux-md-viewer"]:
                    actual = run(str(directory / binary)).strip()
                    assert actual == "FRESH", (name, directory, binary, actual)
            print(f"INSTALL_ARTIFACT_OK {name} {profile}", flush=True)
        # Failed builds must leave the previously installed bundle and CLIs intact.
        installed = [directory / binary for directory in [bins, bundle / "Contents/MacOS"] for binary in ["flowmux", "flowmuxctl", "flowmux-md-viewer"]]
        before = [path.read_bytes() for path in installed]
        # Packaging must not install, launch, or execute the CLI's hook refresh.
        marker = root / "cli-was-executed"
        (source / "flowmux-cli/src/main.rs").write_text('''fn main() {
    if let Ok(path) = std::env::var("FM_TEST_CLI_MARKER") {
        std::fs::write(path, "executed").unwrap();
    }
    println!("FRESH");
}
''')
        package_env = dict(env, FM_TEST_CLI_MARKER=str(marker))
        command = ["bash", str(source / "scripts/install-macos.sh"), "--release", "--bundle-only",
                   "--app-dir", str(apps), "--bin-dir", str(bins)]
        run(*command, cwd=root, env=package_env)
        assert not marker.exists(), "bundle-only executed the CLI"
        assert [path.read_bytes() for path in installed] == before
        packaged = source / "target/macos-bundle/Flowmux.app"
        image = root / "disk images/flowmux.dmg"
        run("bash", str(REPO / "scripts/package-macos.sh"), str(packaged), str(image))
        run("shasum", "-a", "256", "-c", image.name + ".sha256", cwd=image.parent)
        mount = root / "mounted"
        run("hdiutil", "attach", str(image), "-readonly", "-nobrowse", "-mountpoint", str(mount))
        copied = root / "Applications/Flowmux.app"
        try:
            assert (mount / "Applications").is_symlink()
            assert os.readlink(mount / "Applications") == "/Applications"
            assert (mount / "README.md").read_bytes() == (REPO / "packaging/macos/README.md").read_bytes()
            run("ditto", str(mount / "Flowmux.app"), str(copied))
        finally:
            run("hdiutil", "detach", str(mount))
        run("codesign", "--verify", "--deep", "--strict", str(copied))
        for binary in ["flowmux", "flowmuxctl", "flowmux-md-viewer"]:
            assert run(str(copied / "Contents/MacOS" / binary)).strip() == "FRESH"
        rejected = subprocess.run(command + ["--launch"], cwd=root, env=package_env,
                                  capture_output=True, timeout=120)
        assert rejected.returncode != 0, "bundle-only must reject --launch"
        assert not marker.exists()
        print("BUNDLE_ONLY_DMG_INSTALL_OK", flush=True)
        # Release packaging must not silently regenerate an out-of-date lockfile.
        manifest = source / "flowmux-cli/Cargo.toml"
        manifest.write_text(manifest.read_text().replace('version = "0.0.0"', 'version = "0.0.1"'))
        locked = subprocess.run(command, cwd=root, env=package_env, capture_output=True, timeout=120)
        assert locked.returncode != 0 and b"--locked" in locked.stderr, locked.stderr
        assert [path.read_bytes() for path in installed] == before
        print("BUNDLE_ONLY_LOCKFILE_ENFORCED_OK", flush=True)
        (source / "flowmux/src/main.rs").write_text("invalid Rust\n")
        failed = subprocess.run(["bash", str(source / "scripts/install-macos.sh"), "--fast", "--app-dir", str(apps), "--bin-dir", str(bins)], cwd=root, env=env, capture_output=True, timeout=120)
        assert failed.returncode != 0
        assert [path.read_bytes() for path in installed] == before
        print("INSTALL_BUILD_FAILURE_PRESERVES_PREVIOUS_OK", flush=True)


if __name__ == "__main__":
    main()
