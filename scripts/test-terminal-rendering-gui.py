#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Compare terminal behavior in an isolated live flowmux, never the user's GUI.

uv run --with python-xlib python scripts/test-terminal-rendering-gui.py
Use --gui/--cli for a preserved build and --baseline to record old behavior.
Requires Xvfb and dbus-daemon; the sidebar case also needs the Atspi GI binding.
Reuses the SSH GUI harness's process protection.
"""

import argparse
import importlib.util
import json
import os
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
parser.add_argument("--case", choices=["geometry", "workspace", "scrollback", "sidebar", "panes", "panes-restored"], default="geometry")
args = parser.parse_args()
args.gui = str(Path(args.gui).resolve())
args.cli = str(Path(args.cli).resolve())
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
divider_case = args.case in ("sidebar", "panes", "panes-restored")
if divider_case:
    h.env["GTK_A11Y"] = "atspi"
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
    if divider_case:
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
    process, sock = h.window("terminal-rendering")
    workspace = h.rpc(
        sock, "workspace_create", name="Terminal rendering test", root=str(h.root)
    )["workspace_created"]["id"]
    pane = h.workspace(sock, workspace)["panes"][0]["id"]
    if args.case in ("panes", "panes-restored"):
        pane = h.rpc(sock, "pane_split", pane=pane, direction="vertical")["pane_split_done"]["new_pane"]
        if args.case == "panes-restored":
            h.close_window(process)
            process, sock = h.window("restored-panes")
            fixture.wait_for(lambda: h.rpc(sock, "workspace_focus", workspace=workspace), "split workspace restored")
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
    elif divider_case:
        from Xlib import X, display
        from Xlib.ext import xtest

        d = display.Display(h.env["DISPLAY"])
        d.xfixes_query_version()

        def pane_tab():
            desktop = Atspi.get_desktop(0)
            pending = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
            app = next((node for node in pending if node.get_process_id() == process.pid), None)
            pending = [app] if app else []
            matches = []
            while pending:
                node = pending.pop()
                if node.get_role() == Atspi.Role.PUSH_BUTTON and node.get_name() == h.root.name:
                    matches.append(node)
                pending.extend(node.get_child_at_index(i) for i in range(node.get_child_count()))
            return max(matches, key=lambda n: n.get_component_iface().get_extents(Atspi.CoordType.WINDOW).x) if matches else None

        tab = fixture.wait_for(pane_tab, "terminal tab")

        def bounds():
            return tab.get_component_iface().get_extents(Atspi.CoordType.WINDOW)

        def move(x, y):
            xtest.fake_input(d, X.MotionNotify, x=x, y=y)
            d.sync()
            time.sleep(.08)

        def cursor(x, y):
            move(x, y)
            return tuple(d.xfixes_get_cursor_image(d.screen().root).cursor_image)

        def drag(x, y, dx):
            move(x, y)
            xtest.fake_input(d, X.ButtonPress, 1)
            d.sync()
            for offset in range(0, dx + 1, 4):
                move(x + offset, y)
            xtest.fake_input(d, X.ButtonRelease, 1)
            d.sync()
            time.sleep(.3)

        h.send(sock, pane, "printf '\\033[2J\\033[H'; printf 'SIDEBAR_SELECTION_CHECK %.0s' {1..200}")
        fixture.wait_for(lambda: "SIDEBAR_SELECTION_CHECK" in h.screen(sock, pane), "selection text")
        rect = bounds()
        y = rect.y + 160
        text_cursor = cursor(rect.x + 40, y)
        # Scan from text toward the divider, using the actual cursor boundary.
        edge = next(x for x in range(rect.x + 20, rect.x - 40, -1)
                    if cursor(x, y) != text_cursor)
        resize_cursor = cursor(edge, y)
        assert resize_cursor != text_cursor
        primary = d.intern_atom("PRIMARY")
        assert not d.get_selection_owner(primary), "Test starts without selected text"
        before = bounds().x
        drag(edge + 1, y, 80)
        outside_moved = bounds().x - before
        selected = bool(d.get_selection_owner(primary))
        result = {"outside_cursor_resize_delta": outside_moved, "text_selected": selected}
        print(json.dumps(result), flush=True)
        if not args.baseline:
            assert outside_moved == 0, result
            assert selected, "Dragging immediately outside the resize cursor must select text"
            left = next(x for x in range(edge, edge - 30, -1)
                        if cursor(x, y) != resize_cursor)
            drag(left, y, 80)
            assert bounds().x == before, "Left side of the cursor boundary must not resize either"
            # The resize cursor itself must still start a working divider drag.
            assert cursor(edge, y) == resize_cursor
            drag(edge, y, 80)
            fixture.wait_for(lambda: bounds().x > before + 40, "divider resized from its cursor region")
            h.pass_check(f"{args.case}: divider only resizes inside its resize cursor; adjacent terminal text selects")
        (h.root / f"{args.case}.json").write_text(json.dumps(result))
        d.close()
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
        restored = fixture.wait_for(lambda: h.workspace(sock, workspace), "target workspace restored")
        pane = restored["panes"][0]["id"]
        screen = fixture.wait_for(lambda: h.screen(sock, pane), "restored terminal ready")
        result["latest_restored"] = "H0499" in screen
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
