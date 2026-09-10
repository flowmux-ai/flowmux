#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Test global terminal output search in an isolated live GTK application.

Build flowmux and flowmuxctl, then run:
uv run --with python-xlib --with pillow python scripts/test-terminal-output-search-gui.py
Requires Xvfb and dbus-daemon. Screenshots and IPC traces remain in the printed
artifact directory. Only test-owned processes and XDG directories are used.
"""

import importlib.util
import json
import sys
import time

sys.dont_write_bytecode = True
from pathlib import Path
from types import SimpleNamespace

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
(h.root / "backend").mkdir()
(h.root / "frontend").mkdir()
(h.root / "config/flowmux").mkdir()
(h.root / "config/flowmux/options.json").write_text(
    json.dumps(
        {
            "default_shell": str(clean),
            "terminal_minimap_enabled": False,
            "system_notifications_enabled": False,
        }
    )
)


def windows():
    return [
        w
        for w in d.screen().root.query_tree().children
        if w.get_attributes().map_state == X.IsViewable
    ]


def title(w):
    p = w.get_full_property(d.intern_atom("_NET_WM_NAME"), d.intern_atom("UTF8_STRING"))
    return p.value.decode() if p else str(w.get_wm_name() or "")


def key(name, mods=(), delay=0.07):
    codes = [d.keysym_to_keycode(XK.string_to_keysym(x)) for x in (*mods, name)]
    for c in codes:
        xtest.fake_input(d, X.KeyPress, c)
    for c in reversed(codes):
        xtest.fake_input(d, X.KeyRelease, c)
    d.sync()
    time.sleep(delay)


def shot(name):
    root = d.screen().root
    g = root.get_geometry()
    raw = root.get_image(0, 0, g.width, g.height, X.ZPixmap, 0xFFFFFFFF)
    Image.frombytes("RGB", (g.width, g.height), raw.data, "raw", "BGRX").save(
        h.root / name
    )


try:
    h.start_display()
    proc, sock = h.window("search")
    d = display.Display(h.env["DISPLAY"])
    w1 = h.rpc(
        sock, "workspace_create", name="Backend logs", root=str(h.root / "backend")
    )["workspace_created"]["id"]
    w2 = h.rpc(
        sock, "workspace_create", name="Frontend logs", root=str(h.root / "frontend")
    )["workspace_created"]["id"]
    a = h.workspace(sock, w1)["panes"][0]
    p1 = a["id"]
    s1 = a["tabs"][0]["id"]
    b = h.workspace(sock, w2)["panes"][0]
    p2 = b["id"]
    emit = h.root / "emit.py"
    emit.write_text(
        "print('FMNEEDLE_FIRST 한글 오류',flush=True)\nfor n in range(150):print(f'backend filler {n}')\nprint('FMNEEDLE_SECOND 오류',flush=True)\nfor n in range(150):print(f'backend tail {n}')\n"
    )
    h.send(sock, p1, "python3 " + str(emit))
    m.wait_for(lambda: "backend tail 149" in h.screen(sock, p1), "backend output")
    emit2 = h.root / "emit-other.py"
    emit2.write_text("print('FMNEEDLE_OTHER frontend',flush=True)\n")
    h.send(sock, p2, "python3 " + str(emit2))
    m.wait_for(lambda: "FMNEEDLE_OTHER" in h.screen(sock, p2), "frontend output")
    h.rpc(
        sock,
        "surface_create",
        workspace=w1,
        cwd=str(h.root / "backend"),
        shell=str(clean),
    )
    h.rpc(sock, "workspace_focus", workspace=w2)
    main = m.wait_for(
        lambda: next(
            (
                w
                for w in windows()
                if w.get_full_property(d.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                is not None
                and int(
                    w.get_full_property(
                        d.intern_atom("_NET_WM_PID"), X.AnyPropertyType
                    ).value[0]
                )
                == proc.pid
            ),
            None,
        ),
        "main X window",
    )
    main.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    # The footer's Files button is followed immediately by the search button.
    geometry = main.get_geometry()
    xtest.fake_input(d, X.MotionNotify, x=geometry.x + 244, y=geometry.y + geometry.height - 26)
    d.sync()
    time.sleep(0.8)
    shot("side-panel-search-button.png")
    xtest.fake_input(d, X.ButtonPress, 1)
    xtest.fake_input(d, X.ButtonRelease, 1)
    d.sync()
    button_dialog = m.wait_for(
        lambda: next((w for w in windows() if title(w) == "Search all terminals"), None),
        "side panel search button opens dialog",
    )
    button_dialog.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    key("Escape")
    m.wait_for(lambda: all(title(w) != "Search all terminals" for w in windows()), "button dialog closes")
    h.pass_check("Magnifying-glass button right of Files opens global terminal search")
    main.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    key("f", ("Control_L", "Alt_L", "Shift_L"))
    dialog = m.wait_for(
        lambda: next(
            (w for w in windows() if title(w) == "Search all terminals"), None
        ),
        "search shortcut opens dialog",
    )
    dialog.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    for ch in "fmneedle":
        key(ch)
    time.sleep(1.2)
    shot("search-results.png")
    assert h.rpc(sock, "workspace_current")["workspace_current"]["id"] == w2
    assert not next(
        t for p in h.workspace(sock, w1)["panes"] for t in p["tabs"] if t["id"] == s1
    )["active"]
    h.pass_check(
        "Search shortcut opens over foreground workspace without activating hidden target"
    )
    key("Return")
    m.wait_for(
        lambda: all(title(w) != "Search all terminals" for w in windows()),
        "Enter opens selected result",
    )
    m.wait_for(
        lambda: h.rpc(sock, "workspace_current")["workspace_current"]["id"] == w1,
        "result activates correct workspace",
    )
    assert next(
        t for p in h.workspace(sock, w1)["panes"] for t in p["tabs"] if t["id"] == s1
    )["active"]
    screen = h.screen(sock, p1)
    (h.root / "selected-screen.txt").write_text(screen)
    assert "FMNEEDLE_FIRST" in screen and "backend tail 149" not in screen, screen
    time.sleep(0.8)
    shot("selected-output.png")
    h.pass_check(
        "Enter activates hidden tab and scrolls to first retained output match"
    )
    key("f", ("Control_L", "Alt_L", "Shift_L"))
    dialog = m.wait_for(
        lambda: next(
            (w for w in windows() if title(w) == "Search all terminals"), None
        ),
        "reopen search",
    )
    dialog.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    for ch in "zznotpresent":
        key(ch)
    time.sleep(0.6)
    shot("no-results.png")
    key("Escape")
    m.wait_for(
        lambda: all(title(w) != "Search all terminals" for w in windows()),
        "Escape closes search",
    )
    h.pass_check("No-result search and Escape leave the terminal workspace intact")
    # Long wrapped log lines must select the query at the far end.
    huge = h.root / "emit-huge.py"
    huge.write_text(
        "print('abcdefgh'*10000+' HUGENATIVE 한글')\nfor n in range(100):print(f'huge tail {n}')\n"
    )
    h.send(sock, p1, "python3 " + str(huge))
    m.wait_for(lambda: "huge tail 99" in h.screen(sock, p1), "huge output")
    h.rpc(sock, "workspace_focus", workspace=w2)
    main.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    key("f", ("Control_L", "Alt_L", "Shift_L"))
    dialog = m.wait_for(
        lambda: next(
            (w for w in windows() if title(w) == "Search all terminals"), None
        ),
        "long-line search",
    )
    dialog.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    for ch in "hugenative":
        key(ch)
    time.sleep(1)
    shot("huge-results.png")
    key("Return")
    m.wait_for(
        lambda: all(title(w) != "Search all terminals" for w in windows()),
        "long-line result navigation",
    )
    m.wait_for(
        lambda: h.rpc(sock, "workspace_current")["workspace_current"]["id"] == w1,
        "long-line workspace",
    )
    time.sleep(0.5)
    screen = h.screen(sock, p1)
    (h.root / "huge-selected-screen.txt").write_text(screen)
    assert "HUGENATIVE" in screen and "huge tail 99" not in screen, screen
    shot("huge-selected.png")
    h.pass_check("80,000-character line scrolls to the matched text at its far end")
    # Enough history to yield during activation, then real Escape keyboard input.
    cancel = h.root / "emit-cancel.py"
    cancel.write_text(
        "for n in range(8000):print(f'cancel padding {n}')\nprint('CANCEL_TARGET')\nfor n in range(100):print(f'cancel tail {n}')\n"
    )
    h.send(sock, p1, "python3 " + str(cancel))
    m.wait_for(lambda: "cancel tail 99" in h.screen(sock, p1), "cancel output")
    h.rpc(sock, "workspace_focus", workspace=w2)
    main.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    key("f", ("Control_L", "Alt_L", "Shift_L"))
    dialog = m.wait_for(
        lambda: next(
            (w for w in windows() if title(w) == "Search all terminals"), None
        ),
        "cancel search",
    )
    dialog.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    for ch in "target":
        key(ch)
    time.sleep(1.2)
    shot("cancel-results.png")
    key("Return", delay=0.003)
    key("Escape")
    m.wait_for(
        lambda: all(title(w) != "Search all terminals" for w in windows()),
        "Escape during navigation",
    )
    time.sleep(0.7)
    assert h.rpc(sock, "workspace_current")["workspace_current"]["id"] == w2
    shot("cancelled-navigation.png")
    h.pass_check("Escape during result validation cancels workspace navigation")
    h.close_window(proc)
finally:
    for child in reversed(h.children):
        h.stop(child)
    h.events.close()
