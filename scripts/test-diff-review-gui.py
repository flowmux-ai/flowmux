#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise the installed Linux GUI via X11 input and its real IPC server.

Pass --gui and --cli to test a specific installation. Requires Xvfb,
dbus-daemon, python3-xlib and Pillow. Only fixture-owned processes are stopped.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import time

from PIL import Image
from Xlib import X, XK, display, protocol
from Xlib.ext import xtest

sys.dont_write_bytecode = True
repo = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("fixture", repo / "scripts/test-ssh-workspace-gui.py")
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
parser = argparse.ArgumentParser()
parser.add_argument("--gui", required=True)
parser.add_argument("--cli", required=True)
args = parser.parse_args()
args.protected_pid = []
h = m.Harness(args)
h.env["WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS"] = "1"  # isolated root containers
project = h.root / "project"
project.mkdir()
subprocess.run(["git", "init", "-b", "main", str(project)], check=True, capture_output=True)
for n in range(600):
    (project / f"file-{n:04}.rs").write_text(f"fn fixture_{n}() {{}}\n")
(project / "large.rs").write_text("".join(f"line {n} 내용\n" for n in range(20000)))
config = h.root / "config/flowmux"
config.mkdir()
(config / "options.json").write_text(json.dumps({"default_shell": "/bin/bash", "system_notifications_enabled": False}))


def named_window(name):
    for w in d.screen().root.query_tree().children:
        title = w.get_full_property(d.intern_atom("_NET_WM_NAME"), d.intern_atom("UTF8_STRING"))
        if title and title.value.decode() == name and w.get_attributes().map_state == X.IsViewable:
            return w


def key(name, mods=()):
    codes = [d.keysym_to_keycode(XK.string_to_keysym(k)) for k in (*mods, name)]
    for code in codes:
        xtest.fake_input(d, X.KeyPress, code)
    for code in reversed(codes):
        xtest.fake_input(d, X.KeyRelease, code)
    d.sync()


def shot(name):
    root = d.screen().root
    g = root.get_geometry()
    raw = root.get_image(0, 0, g.width, g.height, X.ZPixmap, 0xFFFFFFFF)
    Image.frombytes("RGB", (g.width, g.height), raw.data, "raw", "BGRX").save(h.root / name)


try:
    h.start_display()
    proc, sock = h.window("diff-review")
    workspace = h.rpc(sock, "workspace_create", name="Review fixture", root=str(project))["workspace_created"]["id"]
    h.rpc(sock, "workspace_focus", workspace=workspace)
    pane = h.workspace(sock, workspace)["panes"][0]["id"]
    h.rpc(sock, "pane_send_keys", pane=pane, keys="UNSUBMITTED_DRAFT_한글")
    m.wait_for(lambda: "UNSUBMITTED_DRAFT" in h.screen(sock, pane), "draft entered")
    d = display.Display(h.env["DISPLAY"])
    main = m.wait_for(lambda: next((w for w in d.screen().root.query_tree().children if w.get_attributes().map_state == X.IsViewable), None), "main window")
    main.set_input_focus(X.RevertToParent, X.CurrentTime)
    key("d", ("Control_L", "Alt_L"))
    review = m.wait_for(lambda: named_window("Diff review"), "shortcut opens review")
    time.sleep(1)
    shot("review-small.png")
    assert "UNSUBMITTED_DRAFT" in h.screen(sock, pane)
    assert proc.poll() is None
    review.configure(width=740, height=560)
    review.set_input_focus(X.RevertToParent, X.CurrentTime)
    d.sync()
    time.sleep(0.3)
    shot("review-narrow.png")
    review.send_event(protocol.event.ClientMessage(window=review,
        client_type=d.intern_atom("WM_PROTOCOLS"),
        data=(32, [d.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0])))
    d.sync()
    m.wait_for(lambda: named_window("Diff review") is None, "review closes")
    main.set_input_focus(X.RevertToParent, X.CurrentTime)
    key("d", ("Control_L", "Alt_L"))
    m.wait_for(lambda: named_window("Diff review"), "review reopens")
    assert "UNSUBMITTED_DRAFT" in h.screen(sock, pane)
    h.pass_check("installed GUI opens, resizes and reopens review without altering terminal draft")
    h.close_window(proc)
    print("DIFF_REVIEW_INSTALLED_GUI_OK", flush=True)
finally:
    for child in reversed(h.children):
        h.stop(child)
    h.events.close()
