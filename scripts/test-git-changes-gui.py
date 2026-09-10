#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Live Changes tests in a separate flowmux process, Xvfb, D-Bus and XDG tree.
Run after building: uv run --with python-xlib --with pillow python scripts/test-git-changes-gui.py
Existing flowmux processes are recorded and checked alive throughout cleanup.
"""

import importlib.util
import json
import subprocess
import sys
import time
from pathlib import Path
from types import SimpleNamespace

sys.dont_write_bytecode = True
from PIL import Image
from Xlib import XK, X, display
from Xlib.ext import xtest

repo = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location(
    "fixture", repo / "scripts/test-ssh-workspace-gui.py"
)
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
protected = []
for entry in Path("/proc").iterdir():
    if entry.name.isdigit():
        try:
            if (entry / "comm").read_text().strip() == "flowmux":
                protected.append(int(entry.name))
        except OSError:
            pass
h = m.Harness(
    SimpleNamespace(
        gui=str(repo / "target/debug/flowmux"),
        cli=str(repo / "target/debug/flowmuxctl"),
        protected_pid=protected,
    )
)
h.env["GTK_USE_PORTAL"] = "0"
clean = h.root / "clean-shell"
clean.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
clean.chmod(0o755)
(h.root / "config/flowmux").mkdir()
(h.root / "config/flowmux/options.json").write_text(
    json.dumps(
        {
            "default_shell": str(clean),
            "system_notifications_enabled": False,
            "terminal_minimap_enabled": False,
        }
    )
)
work = h.root / "review-repo"
work.mkdir()
other = h.root / "other-repo"
other.mkdir()


def git(*args, root=work):
    return subprocess.check_output(["git", *args], cwd=root, stderr=subprocess.STDOUT)


for root in [work, other]:
    git("init", "-b", "main", root=root)
    git("config", "user.name", "Test", root=root)
    git("config", "user.email", "test@example.test", root=root)
    (root / "review.txt").write_text("HEAD original 한글\n")
    git("add", "review.txt", root=root)
    git("-c", "commit.gpgsign=false", "commit", "--no-verify", "-m", "base", root=root)
(work / "review.txt").write_text("INDEX staged 한글\n")
git("add", "review.txt")
(work / "review.txt").write_text("WORKING saved 한글\n")
(other / "review.txt").write_text("OTHER workspace\n")


def windows():
    return [
        w
        for w in d.screen().root.query_tree().children
        if w.get_attributes().map_state == X.IsViewable
    ]


def title(w):
    prop = w.get_full_property(
        d.intern_atom("_NET_WM_NAME"), d.intern_atom("UTF8_STRING")
    )
    return prop.value.decode() if prop else str(w.get_wm_name() or "")


def focus(w):
    w.configure(stack_mode=X.Above)
    w.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()


def key(name, mods=(), delay=0.1):
    codes = [d.keysym_to_keycode(XK.string_to_keysym(x)) for x in (*mods, name)]
    for c in codes:
        xtest.fake_input(d, X.KeyPress, c)
    for c in reversed(codes):
        xtest.fake_input(d, X.KeyRelease, c)
    d.sync()
    time.sleep(delay)


def click(w, x, y):
    geo = w.get_geometry()
    xtest.fake_input(d, X.MotionNotify, x=geo.x + x, y=geo.y + y)
    xtest.fake_input(d, X.ButtonPress, 1)
    xtest.fake_input(d, X.ButtonRelease, 1)
    d.sync()
    time.sleep(0.5)


def shot(name):
    root = d.screen().root
    g = root.get_geometry()
    raw = root.get_image(0, 0, g.width, g.height, X.ZPixmap, 0xFFFFFFFF)
    Image.frombytes("RGB", (g.width, g.height), raw.data, "raw", "BGRX").save(
        h.root / name
    )


def changes_window():
    return next((w for w in windows() if title(w) == "Git Changes"), None)


try:
    h.start_display()
    proc, sock = h.window("changes")
    d = display.Display(h.env["DISPLAY"])
    ws = h.rpc(sock, "workspace_create", name="Review", root=str(work))[
        "workspace_created"
    ]["id"]
    ws2 = h.rpc(sock, "workspace_create", name="Other", root=str(other))[
        "workspace_created"
    ]["id"]
    h.rpc(sock, "workspace_focus", workspace=ws)
    pane = h.workspace(sock, ws)["panes"][0]["id"]
    m.wait_for(lambda: "bash-" in h.screen(sock, pane), "review terminal ready")
    main = m.wait_for(
        lambda: next(
            (
                w
                for w in windows()
                if (
                    p := w.get_full_property(
                        d.intern_atom("_NET_WM_PID"), X.AnyPropertyType
                    )
                )
                is not None
                and int(p.value[0]) == proc.pid
            ),
            None,
        ),
        "main test window",
    )
    focus(main)
    key("g", ("Control_L", "Alt_L", "Shift_L"))
    dialog = m.wait_for(changes_window, "Changes shortcut")
    focus(dialog)
    time.sleep(1)
    shot("01-changes-list.png")
    h.pass_check("Ctrl+Alt+Shift+G opens Git Changes in the isolated application")
    click(dialog, 110, 102)
    time.sleep(1.5)
    shot("02-staged-diff.png")
    # The source is pinned even when another workspace receives focus through IPC.
    h.rpc(sock, "workspace_focus", workspace=ws2)
    focus(dialog)
    click(dialog, dialog.get_geometry().width - 65, 29)
    m.wait_for(lambda: git("diff", "--cached", "--name-only") == b"", "Unstage button")
    assert (work / "review.txt").read_text() == "WORKING saved 한글\n"
    assert git("diff", "--cached", "--name-only", root=other) == b""
    h.pass_check(
        "Unstage preserves the working file and stays pinned to the original worktree"
    )
    time.sleep(0.7)
    click(dialog, 110, 102)
    time.sleep(0.7)
    shot("03-unstaged-diff.png")
    click(dialog, dialog.get_geometry().width - 65, 29)
    m.wait_for(
        lambda: git("show", ":review.txt").decode() == "WORKING saved 한글\n",
        "Stage button",
    )
    assert git("diff", "--name-only") == b""
    h.pass_check("Stage writes the selected saved file to the index")
    # Close only the Changes window; the application and both workspaces survive.
    focus(dialog)
    key("Escape")
    m.wait_for(lambda: changes_window() is None, "Escape closes Changes")
    assert h.rpc(sock, "ping")
    assert h.rpc(sock, "workspace_current")["workspace_current"]["id"] == ws2
    h.pass_check("Escape closes only Changes and leaves the test application running")
    h.rpc(sock, "workspace_focus", workspace=ws)
    focus(main)
    key("p", ("Control_L", "Shift_L"))
    time.sleep(0.4)
    for ch in "git changes":
        key("space" if ch == " " else ch, delay=0.03)
    time.sleep(0.3)
    key("Return")
    dialog = m.wait_for(changes_window, "command palette Git Changes")
    focus(dialog)
    shot("04-command-palette-entry.png")
    h.pass_check("Command palette Git Changes opens the same screen")
    key("Escape")
    m.wait_for(lambda: changes_window() is None, "palette Changes closes")
    focus(main)
    click(main, 210, main.get_geometry().height - 26)
    dialog = m.wait_for(changes_window, "side panel Changes button")
    focus(dialog)
    time.sleep(0.5)
    shot("05-side-panel-entry.png")
    h.pass_check("Side panel Changes button opens the same screen")
    key("Escape")
    h.assert_protected()
    print("PROTECTED FLOWMUX PIDS:", protected, flush=True)
    print("PASS: all live Git Changes scenarios", flush=True)
finally:
    if "d" in globals():
        shot("final.png")
        d.close()
    h.cleanup()
