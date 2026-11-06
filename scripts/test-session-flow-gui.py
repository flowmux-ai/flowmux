#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check Fork session with Session Flow hidden in a disposable Linux GUI."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import sqlite3
import subprocess
import sys
import time
import uuid

from Xlib import X, XK, display
from Xlib.ext import xtest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "gui", Path(__file__).with_name("test-ssh-workspace-gui.py"))
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for name in ("gui", "cli"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = gui.Harness(args)
    connection = None
    try:
        h.env.update(GTK_A11Y="atspi", CLAUDE_CONFIG_DIR=str(h.root / "claude-home"))
        h.env.pop("AT_SPI_BUS_ADDRESS", None)
        history = Path(h.env["CLAUDE_CONFIG_DIR"]) / "projects/fixture"
        history.mkdir(parents=True)
        shell = h.root / "clean-shell"
        shell.write_text('#!/bin/sh\nif [ "$1" = "-lc" ]; then exec /bin/sh -c "$2"; fi\n'
                         'exec /bin/bash --noprofile --norc "$@"\n')
        shell.chmod(0o755)
        h.env["SHELL"] = str(shell)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell)}))
        source = h.root / "agent.c"
        source.write_text(r'''#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "--help")) {
        puts("--resume --fork-session");
        return 0;
    }
    printf("fixture-pid:%d\n❯ \n", getpid());
    fflush(stdout);
    while (getchar() != EOF) {}
    return 0;
}
''')
        executable = h.root / "claude"
        subprocess.run(["cc", str(source), "-o", str(executable)], check=True, timeout=30)
        h.env["PATH"] = str(h.root) + os.pathsep + h.env["PATH"]
        h.start_display()
        os.environ["DISPLAY"] = h.env["DISPLAY"]
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ.pop("AT_SPI_BUS_ADDRESS", None)
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        connection = display.Display(h.env["DISPLAY"])
        process, socket = h.window("session-flow")

        def mapped_window():
            return next((window for window in connection.screen().root.query_tree().children
                         if (owner := window.get_full_property(connection.intern_atom("_NET_WM_PID"),
                                                              X.AnyPropertyType)) is not None
                         and int(owner.value[0]) == process.pid
                         and window.get_attributes().map_state == X.IsViewable), None)

        window = gui.wait_for(mapped_window, "mapped fixture window")
        workspace = h.rpc(socket, "workspace_create", name="Fork fixture", root=str(h.root))["workspace_created"]["id"]
        h.rpc(socket, "workspace_focus", workspace=workspace)
        pane = h.workspace(socket, workspace)["panes"][0]
        surface = pane["tabs"][0]["id"]
        pane = pane["id"]
        database = h.root / "data/flowmux/session-flow.sqlite"
        gui.wait_for(database.exists, "initial empty catalog")
        h.send(socket, pane, "exec " + shlex.quote(str(executable)))
        pid = int(gui.wait_for(lambda: re.search(r"fixture-pid:(\d+)", h.screen(socket, pane)),
                              "fixture agent").group(1))

        def find(role, name=None):
            desktop = Atspi.get_desktop(0)
            desktop.clear_cache()
            pending = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
            pending = [node for node in pending if node and node.get_process_id() == process.pid]
            while pending:
                node = pending.pop()
                if node is None:
                    continue
                node.clear_cache()
                if (node.get_role() == role and (name is None or node.get_name() == name)
                        and node.get_state_set().contains(Atspi.StateType.SHOWING)):
                    return node
                pending.extend(node.get_child_at_index(i) for i in range(node.get_child_count()))

        def menu_enabled():
            # Native input needs a mapped X window, not a particular accessibility role.
            bounds = window.get_geometry()
            point = connection.screen().root.translate_coords(
                window, bounds.width * 3 // 4, bounds.height // 2)
            window.set_input_focus(X.RevertToParent, X.CurrentTime)
            xtest.fake_input(connection, X.MotionNotify, x=point.x, y=point.y)
            xtest.fake_input(connection, X.ButtonPress, 3)
            xtest.fake_input(connection, X.ButtonRelease, 3)
            connection.sync()
            button = gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fork session"), "Fork menu")
            enabled = button.get_state_set().contains(Atspi.StateType.SENSITIVE)
            code = connection.keysym_to_keycode(XK.string_to_keysym("Escape"))
            xtest.fake_input(connection, X.KeyPress, code)
            xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()
            gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fork session") is None, "closed menu")
            return enabled

        def report(session, activity="idle", start=False):
            h.rpc(socket, "agent_activity_update", pane=pane, surface=surface, agent="claude",
                  pid=pid, source="flowmux:hook", session_id=session,
                  seq=time.time_ns(), activity=activity, custom_status="Ready" if start else None)
            gui.wait_for(lambda: h.workspace(socket, workspace)["panes"][0]["tabs"][0]
                         .get("agent", {}).get("session_id") == session, "bound session identity")

        def transcript(session):
            (history / f"{session}.jsonl").write_text(json.dumps(dict(
                type="user", sessionId=session, cwd=str(h.root),
                message=dict(role="user", content="Fork fixture"))) + "\n")

        def imported(session):
            with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as db:
                return db.execute("SELECT COUNT(*) FROM sessions WHERE key LIKE ?",
                                  (f"%{session}%",)).fetchone()[0] == 1

        first, second = str(uuid.uuid4()), str(uuid.uuid4())
        transcript(first)
        report(first, start=True)
        gui.wait_for(lambda: imported(first), "new session imported with panel hidden", timeout=12)
        gui.wait_for(menu_enabled, "new idle session fork enabled", timeout=8)
        h.pass_check("new idle session enables Fork without opening Session Flow")
        report(first, "running")
        gui.wait_for(lambda: not menu_enabled(), "working session fork disabled", timeout=8)
        report(first)
        gui.wait_for(menu_enabled, "completed session fork enabled", timeout=8)
        h.pass_check("working session stays disabled and completion re-enables Fork")
        report(second, start=True)
        gui.wait_for(lambda: not menu_enabled(), "changed session cannot use stale fork", timeout=8)
        # Let the first catalog load finish before the provider saves its transcript.
        time.sleep(1)
        assert not imported(second)
        transcript(second)
        gui.wait_for(lambda: imported(second), "delayed transcript imported with panel hidden", timeout=65)
        gui.wait_for(menu_enabled, "changed session fork enabled", timeout=8)
        h.pass_check("same-pane session replacement and delayed history recover automatically")
        assert find(Atspi.Role.PUSH_BUTTON, "Fit") is None, "Session Flow was opened by the test"
        h.close_window(process)
    finally:
        if connection is not None:
            connection.close()
        h.cleanup()


if __name__ == "__main__":
    main()
