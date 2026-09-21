#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Compare terminal behavior in an isolated live flowmux, never the user's GUI.

uv run --with python-xlib python scripts/test-terminal-rendering-gui.py
Use --gui/--cli for a preserved build and --baseline to record old behavior.
Requires Xvfb and dbus-daemon; reuses the SSH GUI harness's process protection.
"""

import argparse
import importlib.util
import json
from pathlib import Path
import sys
import time

sys.dont_write_bytecode = True
repo = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "fixture", repo / "scripts/test-ssh-workspace-gui.py"
)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--gui", default=str(repo / "target/debug/flowmux"))
parser.add_argument("--cli", default=str(repo / "target/debug/flowmuxctl"))
parser.add_argument("--baseline", action="store_true")
args = parser.parse_args()
args.protected_pid = []
for entry in Path("/proc").iterdir():
    if entry.name.isdigit():
        try:
            if (entry / "comm").read_text().strip() == "flowmux":
                args.protected_pid.append(int(entry.name))
        except OSError:
            pass
h = fixture.Harness(args)
h.env.update(GTK_USE_PORTAL="0", GTK_IM_MODULE="simple")
shell = h.root / "clean-shell"
shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
shell.chmod(0o755)
(h.root / "config/flowmux").mkdir()
(h.root / "config/flowmux/options.json").write_text(
    json.dumps({
        "default_shell": str(shell),
        "terminal_minimap_enabled": True,
        "terminal_minimap_width": 60,
        "cursor_blink": False,
        "usage_bar_enabled": False,
        "system_notifications_enabled": False,
    })
)

try:
    h.start_display()
    process, sock = h.window("terminal-rendering")
    workspace = h.rpc(
        sock, "workspace_create", name="Terminal rendering test", root=str(h.root)
    )["workspace_created"]["id"]
    pane = h.workspace(sock, workspace)["panes"][0]["id"]
    probe = h.root / "geometry.py"
    result_path = h.root / "geometry.json"
    probe.write_text(
        "import json,os,time\n"
        "sizes=[]\n"
        "def record():sizes.append(list(os.get_terminal_size()))\n"
        "time.sleep(.2)\n"
        "record()\n"
        "for _ in range(3):\n"
        " os.write(1,b'\\x1b[?1049h\\x1b[2JALT SCREEN')\n"
        " record();time.sleep(.3);record()\n"
        " os.write(1,b'\\x1b[?1049l')\n"
        " record();time.sleep(.3);record()\n"
        f"open({str(result_path)!r},'w').write(json.dumps(sizes))\n"
    )
    h.send(sock, pane, f"/usr/bin/python3 {probe}")
    fixture.wait_for(result_path.exists, "three alternate-screen round trips")
    sizes = json.loads(result_path.read_text())
    stable = all(size == sizes[0] for size in sizes)
    print(json.dumps({"sizes": sizes, "stable": stable}), flush=True)
    if not args.baseline:
        assert stable, f"Screen mode changed PTY geometry: {sizes}"
        h.pass_check("Normal/alternate transitions preserve terminal geometry")
    h.close_window(process)
finally:
    for child in reversed(h.children):
        h.stop(child)
    h.events.close()
