#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Measure X11 key delivery while hidden terminal agents continuously repaint.

Uses the shared isolated Xvfb/D-Bus harness. This measures XTest-to-PTY delivery,
not hardware keyboard latency or display presentation. Requires python3-xlib.
"""

import argparse
from contextlib import closing
import importlib.util
import json
from pathlib import Path
import shlex
import signal
import statistics
import sys
import time

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
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({
            "default_shell": "/bin/sh", "usage_bar_enabled": False,
            "terminal_minimap_enabled": False, "agent_bar_mode": True}))
        h.env.update(CODEX_HOME=str(h.root / "codex-home"),
                     CLAUDE_CONFIG_DIR=str(h.root / "claude-home"), GTK_IM_MODULE="simple")
        worker = h.root / "worker.py"
        worker.write_text("""import os, sys, time
from pathlib import Path
Path(sys.argv[1]).write_text(str(os.getpid()))
print('\\x1b[?1049h', end='')
for tick in range(3000):
    print('\\x1b[H\\x1b[2J' + ('background output ' + '.' * 60 + '\\r\\n') * 12 +
          f'Working ({tick}s - esc to interrupt)\\r\\n', end='', flush=True)
    time.sleep(1 / 30)
""")
        probe = h.root / "input.py"
        probe.write_text("""import json, os, sys, termios, time, tty
from pathlib import Path
old = termios.tcgetattr(0)
try:
    tty.setraw(0)
    Path(sys.argv[1]).touch()
    with open(sys.argv[2], 'w', buffering=1) as log:
        while True:
            data = os.read(0, 4096)
            if not data: break
            log.write(json.dumps([time.monotonic(), data.decode('ascii')]) + '\\n')
            os.write(1, data)
finally:
    termios.tcsetattr(0, termios.TCSANOW, old)
""")
        h.start_display()
        process, socket = h.window("terminal-load")
        background_surfaces = []
        for index in range(8):
            workspace = h.rpc(socket, "workspace_create", name=f"Background {index}",
                              root=str(h.root))["workspace_created"]["id"]
            pane = h.workspace(socket, workspace)["panes"][0]
            tab = pane["tabs"][0]
            background_surfaces.append(tab["id"])
            pidfile = h.root / f"worker-{index}.pid"
            h.send(socket, pane["id"], shlex.join([sys.executable, str(worker), str(pidfile)]))
            gui.wait_for(pidfile.exists, "background output started")
            h.rpc(socket, "agent_activity_update", pane=pane["id"], surface=tab["id"],
                  agent="codex", activity="running", pid=int(pidfile.read_text()),
                  source="flowmux:hook", session_id=f"load-{index}")
        workspace = h.rpc(socket, "workspace_create", name="Input", root=str(h.root))["workspace_created"]["id"]
        pane = h.workspace(socket, workspace)["panes"][0]["id"]
        h.rpc(socket, "workspace_focus", workspace=workspace)
        h.rpc(socket, "pane_focus", pane=pane)
        ready, received = h.root / "ready", h.root / "received.jsonl"
        h.send(socket, pane, shlex.join([sys.executable, str(probe), str(ready), str(received)]))
        gui.wait_for(ready.exists, "foreground input probe ready")
        def background_statuses():
            tabs = {tab["id"]: tab for workspace in h.rpc(socket, "workspace_tree")["tree"]["workspaces"]
                    for pane in workspace["panes"] for tab in pane["tabs"]}
            return [(tabs[surface].get("agent") or {}).get("custom_status")
                    for surface in background_surfaces]
        def statuses_ready():
            statuses = background_statuses()
            return statuses if all(status and status.startswith("Working (") for status in statuses) else None
        initial_statuses = gui.wait_for(statuses_ready, "all background agent displays receive output status")
        with closing(display.Display(h.env["DISPLAY"])) as connection:
            def own_window():
                for window in connection.screen().root.query_tree().children:
                    pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                    if (pid is not None and int(pid.value[0]) == process.pid
                            and window.get_attributes().map_state == X.IsViewable):
                        return window
                return None
            window = gui.wait_for(own_window, "isolated test window")
            window.set_input_focus(X.RevertToParent, X.CurrentTime)
            connection.sync()
            time.sleep(1)
            sent = []
            before = Path(f"/proc/{process.pid}/schedstat").read_text().split()
            started = time.monotonic()
            for index in range(400):
                char = "abcdefghijklmnopqrstuvxyz"[index % 25]
                key = connection.keysym_to_keycode(XK.string_to_keysym(char))
                sent.append((time.monotonic(), char))
                xtest.fake_input(connection, X.KeyPress, key)
                xtest.fake_input(connection, X.KeyRelease, key)
                connection.flush()
                time.sleep(.04)
            def input_complete():
                rows = [json.loads(line) for line in received.read_text().splitlines()]
                return rows if sum(len(row[1]) for row in rows) >= len(sent) else None
            rows = gui.wait_for(input_complete, "every key reached the foreground PTY")
            actual = [(stamp, char) for stamp, text in rows for char in text]
            assert ''.join(char for _, char in actual) == ''.join(char for _, char in sent), "lost, reordered or duplicated input"
            elapsed = time.monotonic() - started
            after = Path(f"/proc/{process.pid}/schedstat").read_text().split()
            delays = sorted((got[0] - expected[0]) * 1000 for got, expected in zip(actual, sent))
            result = {"keys": len(sent), "background_terminals": 8,
                      "latency_ms": {"median": statistics.median(delays), "p95": delays[380],
                                     "p99": delays[396], "max": delays[-1]},
                      "main_thread_cpu_percent": (int(after[0]) - int(before[0])) / 1e9 / elapsed * 100}
            (h.root / "result.json").write_text(json.dumps(result, indent=2))
            print(json.dumps(result), flush=True)
            assert delays[-1] < 1000, f"input stalled for a second: {result}"
            tail = ''.join(char for _, char in sent[-25:])
            gui.wait_for(lambda: tail in h.screen(socket, pane).replace('\n', ''), "typed text visible in terminal")
            assert h.rpc(socket, "workspace_current")["workspace_current"]["id"] == workspace
            final_statuses = background_statuses()
            assert all(new and new != old for old, new in zip(initial_statuses, final_statuses)), "background status updates stopped"
            h.pass_check("400 native key presses arrive once, in order, and are visible under background output")
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
