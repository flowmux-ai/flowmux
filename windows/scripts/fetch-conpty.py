#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Copy the hash-pinned official ConPTY package using its native SDK layout."""
import argparse
import hashlib
import io
import json
from pathlib import Path
import urllib.request
import zipfile

parser = argparse.ArgumentParser()
parser.add_argument("--output", required=True, type=Path)
args = parser.parse_args()
root = Path(__file__).resolve().parents[1]
lock = json.loads((root / "conpty.lock.json").read_text())
cache = root / "dist/dependencies" / (lock["package"] + "." + lock["version"] + ".nupkg")
if cache.exists():
    data = cache.read_bytes()
else:
    data = urllib.request.urlopen(lock["url"], timeout=60).read()
if hashlib.sha256(data).hexdigest() != lock["sha256"]:
    raise RuntimeError("ConPTY package SHA-256 mismatch; refusing to use it")
if not cache.exists():
    cache.parent.mkdir(parents=True, exist_ok=True)
    cache.write_bytes(data)
with zipfile.ZipFile(io.BytesIO(data)) as archive:
    for source, destination in lock["files"].items():
        content = archive.read(source)
        target = args.output / destination
        if target.exists() and target.read_bytes() == content:
            continue
        target.parent.mkdir(parents=True, exist_ok=True)
        temporary = target.with_suffix(target.suffix + ".tmp")
        temporary.write_bytes(content)
        temporary.replace(target)
print(f"Verified ConPTY {lock['version']} -> {args.output}")
