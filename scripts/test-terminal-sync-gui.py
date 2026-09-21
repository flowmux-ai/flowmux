#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Probe DEC 2026 atomicity and query latency in an isolated live flowmux.

uv run --with python-xlib --with pillow python scripts/test-terminal-sync-gui.py
AUDIT_GUI and AUDIT_CLI select before/after binaries. Results are observational.
"""
import importlib.util
import json
import os
from pathlib import Path
import sys
import time
from types import SimpleNamespace

from PIL import Image, ImageChops
from Xlib import X, display

sys.dont_write_bytecode = True
repo = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("fixture", repo / "scripts/test-ssh-workspace-gui.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
protected = []
for entry in Path("/proc").iterdir():
    try:
        if entry.name.isdigit() and (entry / "comm").read_text().strip() == "flowmux":
            protected.append(int(entry.name))
    except OSError:
        pass
h = fixture.Harness(SimpleNamespace(
    gui=os.environ.get("AUDIT_GUI", str(repo / "target/debug/flowmux")),
    cli=os.environ.get("AUDIT_CLI", str(repo / "target/debug/flowmuxctl")),
    protected_pid=protected,
))
h.env.update(GTK_USE_PORTAL="0", GTK_IM_MODULE="simple")
shell = h.root / "shell"
shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
shell.chmod(0o755)
(h.root / "config/flowmux").mkdir()
(h.root / "config/flowmux/options.json").write_text(json.dumps({
    "default_shell": str(shell), "terminal_minimap_enabled": False,
    "cursor_blink": False, "usage_bar_enabled": False,
    "system_notifications_enabled": False,
}))
probe = h.root / "sync-probe.py"
probe.write_text(r'''
import json, os, select, sys, time, tty
from pathlib import Path
root = Path(sys.argv[1])
tty.setraw(0)
def wait(name):
    deadline = time.monotonic() + 10
    while not (root / name).exists():
        assert time.monotonic() < deadline, name
        time.sleep(.001)
os.write(1, b'\x1b[?1049h\x1b[?25l')
for case in ['small', 'query', 'large']:
    os.write(1, b'\x1b[2J\x1b[HOLD FRAME')
    (root / (case + '-ready')).touch()
    wait(case + '-go')
    for byte in b'\x1b[?2026h':
        os.write(1, bytes([byte]))
        time.sleep(.001)
    os.write(1, b'\x1b[2J\x1b[H')
    if case == 'large':
        os.write(1, b'\x1b[H' * 110000)
    (root / (case + '-begun')).touch()
    if case == 'query':
        start = time.monotonic()
        os.write(1, b'\x1b[6n')
        reply = os.read(0, 4096) if select.select([0], [], [], 1)[0] else b''
        (root / 'query.json').write_text(json.dumps({
            'latency_ms': (time.monotonic() - start) * 1000, 'reply': reply.hex(),
        }))
    wait(case + '-end')
    os.write(1, b'NEW FRAME\x1b[?2026l')
    (root / (case + '-done')).touch()
    wait(case + '-next')
os.write(1, b'\x1b[?1049l\x1b[?25h')
''')
try:
    h.start_display()
    process, sock = h.window("synchronized-output")
    workspace = h.rpc(sock, "workspace_create", name="sync probe", root=str(h.root))["workspace_created"]["id"]
    pane = h.workspace(sock, workspace)["panes"][0]["id"]
    d = display.Display(h.env["DISPLAY"])
    window = fixture.wait_for(lambda: next((w for w in d.screen().root.query_tree().children
        if w.get_attributes().map_state == X.IsViewable
        and (pid := w.get_full_property(d.intern_atom("_NET_WM_PID"), X.AnyPropertyType)) is not None
        and int(pid.value[0]) == process.pid), None), "test window")
    def shot(name):
        geometry = window.get_geometry()
        raw = window.get_image(0, 0, geometry.width, geometry.height, X.ZPixmap, 0xffffffff)
        frame = Image.frombytes("RGB", (geometry.width, geometry.height), raw.data, "raw", "BGRX")
        frame.save(h.root / (name + ".png"))
        return frame.crop((287, 90, geometry.width - 5, geometry.height - 5))
    h.send(sock, pane, f"/usr/bin/python3 {probe} {h.root}")
    results = {}
    for case in ["small", "query", "large"]:
        fixture.wait_for(lambda: (h.root / (case + "-ready")).exists(), case + " ready")
        fixture.wait_for(lambda: "OLD FRAME" in h.screen(sock, pane), "old frame rendered")
        time.sleep(.15)
        before = shot(case + "-before")
        (h.root / (case + "-go")).touch()
        fixture.wait_for(lambda: (h.root / (case + "-begun")).exists(), case + " begun")
        time.sleep(.08)
        during = shot(case + "-during")
        results[case] = {
            "old_text_retained": "OLD FRAME" in h.screen(sock, pane),
            "pixels_retained": ImageChops.difference(before, during).getbbox() is None,
        }
        (h.root / (case + "-end")).touch()
        fixture.wait_for(lambda: (h.root / (case + "-done")).exists(), case + " done")
        fixture.wait_for(lambda: "NEW FRAME" in h.screen(sock, pane), "new frame rendered")
        shot(case + "-after")
        (h.root / (case + "-next")).touch()
    results["query"].update(json.loads((h.root / "query.json").read_text()))
    (h.root / "sync.json").write_text(json.dumps(results, indent=2))
    print(json.dumps(results), flush=True)
    h.close_window(process)
finally:
    for child in reversed(h.children):
        h.stop(child)
    h.events.close()
