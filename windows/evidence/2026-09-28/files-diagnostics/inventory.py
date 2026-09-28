# SPDX-License-Identifier: GPL-3.0-or-later
"""Record final F07 artifacts after release, packaging, and docs are frozen.

Usage: python3 windows/dist/inventory-files.py [windows/path/to/native-lib-test.exe]
Run from any working directory. This script never builds or launches artifacts.
"""

import argparse
import hashlib
import json
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("native_lib_test", nargs="?", type=Path)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    target = Path("windows/target/x86_64-pc-windows-msvc")
    installer = Path("windows/dist/flowmux-windows-0.10.1-dev-x64-setup.exe")
    paths = [
        target / mode / name
        for mode in ("debug", "release")
        for name in ("flowmux.exe", "flowmuxctl.exe", "flowmux.com", "flowmux-command.exe")
    ]
    paths += [
        installer,
        target / "release/conpty.dll",
        target / "release/x64/OpenConsole.exe",
        Path("windows/README.md"),
        Path("windows/IMPLEMENTATION.md"),
        Path("windows/acceptance.json"),
        Path("windows/EDITOR.md"),
        Path("windows/installer.nsi"),
        Path("windows/scripts/configure-path.ps1"),
        Path("windows/scripts/install-webview2.ps1"),
    ]
    if args.native_lib_test is not None:
        candidate = args.native_lib_test
        if candidate.is_absolute():
            candidate = candidate.resolve().relative_to(root)
        if ".." in candidate.parts or candidate.suffix.lower() != ".exe":
            parser.error("native_lib_test must be a repository-local .exe path")
        paths.append(candidate)
    assets = root / "windows/assets"
    if not assets.is_dir():
        raise FileNotFoundError(assets)
    paths += sorted(path.relative_to(root) for path in assets.rglob("*") if path.is_file())

    values = []
    seen = set()
    for relative in paths:
        if relative in seen:
            continue
        seen.add(relative)
        source = root / relative
        before = source.stat()
        digest = hashlib.sha256()
        size = 0
        with source.open("rb") as stream:
            for chunk in iter(lambda: stream.read(1024 * 1024), b""):
                size += len(chunk)
                digest.update(chunk)
        after = source.stat()
        if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns) or size != after.st_size:
            raise RuntimeError(f"Artifact changed while hashing: {relative}")
        values.append({"path": relative.as_posix(), "bytes": size, "sha256": digest.hexdigest()})

    output = root / "windows/evidence/2026-09-28/artifacts-files.json"
    # Write only after every required input has been successfully hashed.
    with output.open("w", encoding="utf-8", newline="\n") as stream:
        stream.write(json.dumps(values, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({
        "inventory": str(output),
        "entries": len(values),
        "native_lib_test_included": args.native_lib_test is not None,
        "installer": next(value for value in values if value["path"] == installer.as_posix()),
    }, indent=2))


if __name__ == "__main__":
    main()
