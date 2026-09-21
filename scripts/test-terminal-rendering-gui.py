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
parser.add_argument("--case", choices=["geometry", "workspace", "scrollback"], default="geometry")
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
        "restore_terminal_scrollback": True,
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
    if args.case == "geometry":
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
    elif args.case == "workspace":
        from Xlib import X, XK, display
        from Xlib.ext import xtest

        original = h.workspace(sock, workspace)["panes"][0]["tabs"][0]["id"]
        h.rpc(sock, "surface_create", workspace=workspace, cwd=str(h.root), shell=str(shell))
        h.rpc(sock, "surface_focus", pane=pane, surface=original)
        right = h.rpc(sock, "pane_split", pane=pane, direction="vertical")["pane_split_done"]["new_pane"]
        h.rpc(sock, "pane_focus", pane=right)
        time.sleep(.3)
        h.rpc(sock, "workspace_create", name="Other workspace", root=str(h.root))
        h.rpc(sock, "workspace_focus", workspace=workspace)
        time.sleep(.3)
        tabs = next(p["tabs"] for p in h.workspace(sock, workspace)["panes"] if p["id"] == pane)
        preserved_tab = any(t["id"] == original and t["active"] for t in tabs)
        d = display.Display(h.env["DISPLAY"])
        window = fixture.wait_for(lambda: next((w for w in d.screen().root.query_tree().children
            if w.get_attributes().map_state == X.IsViewable
            and (pid := w.get_full_property(d.intern_atom("_NET_WM_PID"), X.AnyPropertyType)) is not None
            and int(pid.value[0]) == process.pid), None), "test window")
        window.set_input_focus(X.RevertToParent, X.CurrentTime)
        d.sync()
        time.sleep(.2)
        for char in "mrumarker":
            code = d.keysym_to_keycode(XK.string_to_keysym(char))
            xtest.fake_input(d, X.KeyPress, code)
            xtest.fake_input(d, X.KeyRelease, code)
        d.sync()
        time.sleep(.3)
        preserved_focus = "mrumarker" in h.screen(sock, right)
        result = {"preserved_tab": preserved_tab, "preserved_focus": preserved_focus}
        (h.root / "workspace.json").write_text(json.dumps(result))
        print(json.dumps(result), flush=True)
        if not args.baseline:
            assert preserved_tab and preserved_focus, result
            h.pass_check("Workspace return preserves active tabs and last focused pane")
    else:
        from Xlib import X, XK, display
        from Xlib.ext import xtest

        emit = h.root / "history.py"
        emit.write_text("for i in range(500):print('H%04d 한글 history'%i)\n")
        h.send(sock, pane, f"/usr/bin/python3 {emit}")
        fixture.wait_for(lambda: "H0499" in h.screen(sock, pane), "history generated")
        d = display.Display(h.env["DISPLAY"])
        window = fixture.wait_for(lambda: next((w for w in d.screen().root.query_tree().children
            if w.get_attributes().map_state == X.IsViewable
            and (pid := w.get_full_property(d.intern_atom("_NET_WM_PID"), X.AnyPropertyType)) is not None
            and int(pid.value[0]) == process.pid), None), "test window")
        window.set_input_focus(X.RevertToParent, X.CurrentTime)
        d.sync()
        for _ in range(4):
            codes = [d.keysym_to_keycode(XK.string_to_keysym(k)) for k in ("Shift_L", "Page_Up")]
            for code in codes:
                xtest.fake_input(d, X.KeyPress, code)
            for code in reversed(codes):
                xtest.fake_input(d, X.KeyRelease, code)
            d.sync()
            time.sleep(.1)
        assert "H0499" not in h.screen(sock, pane), "Precondition: scrolled away from tail"
        h.close_window(process)
        state = json.loads(h.state_path.read_text())
        def snapshots(value):
            if isinstance(value, dict):
                if value.get("scrollback"):
                    yield value["scrollback"]["content"]
                for child in value.values():
                    yield from snapshots(child)
            elif isinstance(value, list):
                for child in value:
                    yield from snapshots(child)
        saved = list(snapshots(state))
        assert len(saved) == 1, saved
        result = {"old_retained": "H0000" in saved[0], "latest_retained": "H0499" in saved[0]}
        process, sock = h.window("restored-scrollback")
        fixture.wait_for(lambda: h.tree(sock), "workspace restored")
        pane = h.tree(sock)[0]["panes"][0]["id"]
        time.sleep(.5)
        result["latest_restored"] = "H0499" in h.screen(sock, pane)
        (h.root / "scrollback.json").write_text(json.dumps(result))
        print(json.dumps(result), flush=True)
        if not args.baseline:
            assert all(result.values()), result
            h.pass_check("Off-screen history survives saving while scrolled back and GUI restart")
    h.close_window(process)
finally:
    for child in reversed(h.children):
        h.stop(child)
    h.events.close()
