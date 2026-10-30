#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Bounded OOM and signal recovery in an isolated GUI; requires user systemd."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import time

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("gui", Path(__file__).with_name("test-ssh-workspace-gui.py"))
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    args.gui = str(Path(args.gui).resolve(strict=True))
    args.cli = str(Path(args.cli).resolve(strict=True))
    real_bus = os.environ.get("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/%s/bus" % os.getuid())
    h = gui.Harness(args)
    try:
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        options = {"default_shell": str(shell), "pane_memory_max": "64M"}
        (config / "options.json").write_text(json.dumps(options))
        h.start_display()
        private_bus = h.env["DBUS_SESSION_BUS_ADDRESS"]
        # Keep accessibility private while scope creation uses the real user manager.
        address = subprocess.check_output(["gdbus", "call", "--session", "--dest", "org.a11y.Bus",
            "--object-path", "/org/a11y/bus", "--method", "org.a11y.Bus.GetAddress"], env=h.env, text=True)
        import ast
        address = ast.literal_eval(address)[0]
        os.environ["AT_SPI_BUS_ADDRESS"] = address
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ["DISPLAY"] = h.env["DISPLAY"]
        h.env.update(DBUS_SESSION_BUS_ADDRESS=real_bus, AT_SPI_BUS_ADDRESS=address, GTK_A11Y="atspi")
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        process, path = h.window("process-isolation")
        workspace = h.rpc(path, "workspace_create", name="OOM recovery", root=str(h.root))["workspace_created"]["id"]
        h.rpc(path, "workspace_focus", workspace=workspace)
        pane = h.workspace(path, workspace)["panes"][0]["id"]
        original = h.workspace(path, workspace)["panes"][0]["tabs"][0]["id"]

        def find(name):
            desktop = Atspi.get_desktop(0)
            apps = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
            pending = [app for app in apps if app and app.get_process_id() == process.pid]
            while pending:
                node = pending.pop()
                if node:
                    if node.get_name() == name:
                        if name not in ("Close tab", "Keep tab", "Close workspace"):
                            return node
                        parent = node.get_parent()
                        while parent:
                            if parent.get_role_name() in ("dialog", "alert"):
                                return node
                            parent = parent.get_parent()
                    pending.extend(node.get_child_at_index(i) for i in range(node.get_child_count()))

        def click(name):
            node = gui.wait_for(lambda: find(name), name)
            assert node.get_action_iface().do_action(0), name
            gui.wait_for(lambda: not find(name), name + " dismissed")

        def tabs():
            return h.workspace(path, workspace)["panes"][0]["tabs"]

        def add():
            before = {t["id"] for t in tabs()}
            h.rpc(path, "surface_create", workspace=workspace, cwd=None)
            return next(t["id"] for t in tabs() if t["id"] not in before)

        def shell_pid(surface):
            h.rpc(path, "surface_focus", pane=pane, surface=surface)
            target = h.root / (surface + ".pid")
            target.unlink(missing_ok=True)
            h.send(path, pane, "echo $$ > " + shlex.quote(str(target)))
            gui.wait_for(lambda: target.exists() and target.read_text().strip(), "shell PID")
            return int(target.read_text())

        original_pid = shell_pid(original)
        dead = add()
        pid = shell_pid(dead)
        unit = f"flowmux-{process.pid}-{dead}.scope"
        cgroup = Path(f"/proc/{pid}/cgroup").read_text()
        assert unit in cgroup, cgroup
        assert Path(f"/proc/{original_pid}/cgroup").read_text() != cgroup
        assert Path(f"/proc/{pid}/oom_score_adj").read_text() == Path(f"/proc/{process.pid}/oom_score_adj").read_text()
        h.pass_check("terminal tabs enter distinct scopes before shell startup; isolated score is inherited")
        h.send(path, pane, "python3 -c 'a=bytearray(128*1024*1024)'")
        gui.wait_for(lambda: find("A process in this tab was killed: out of memory"), "OOM explanation")
        assert process.poll() is None and gui.alive(original_pid)
        click("Keep tab")
        assert any(t["id"] == dead for t in tabs())
        h.pass_check("bounded OOM leaves GUI and sibling alive; Keep tab retains dead tab")
        h.rpc(path, "surface_close", pane=pane, surface=dead)
        dead = add()
        shell_pid(dead)
        h.send(path, pane, "python3 -c 'a=bytearray(128*1024*1024)'")
        gui.wait_for(lambda: find("A process in this tab was killed: out of memory"), "second OOM explanation")
        click("Close tab")
        gui.wait_for(lambda: len(tabs()) == 1, "only dead tab removed")
        assert tabs()[0]["id"] == original and gui.alive(original_pid)
        h.pass_check("Close tab removes only the OOM victim")
        dead = add()
        shell_pid(dead)
        h.send(path, pane, "kill -KILL $$")
        gui.wait_for(lambda: find("This tab's process was terminated by SIGKILL"), "signal explanation")
        click("Close tab")
        gui.wait_for(lambda: len(tabs()) == 1, "signaled tab removed")
        for code in (0, 130, 137):
            dead = add()
            shell_pid(dead)
            h.send(path, pane, f"exit {code}")
            time.sleep(.5)
            assert not find("Keep tab"), f"normal exit {code} opened a dialog"
            h.rpc(path, "surface_close", pane=pane, surface=dead)
        dead = add()
        shell_pid(dead)
        h.rpc(path, "surface_close", pane=pane, surface=dead)
        time.sleep(.5)
        assert not find("Keep tab")
        h.pass_check("external SIGKILL reports; exit 0/130/137 and manual tab close stay silent")
        split = h.rpc(path, "pane_split", pane=pane, direction="vertical")["pane_split_done"]["new_pane"]
        h.send(path, split, "kill -KILL $$")
        gui.wait_for(lambda: find("This tab's process was terminated by SIGKILL"), "split tab explanation")
        click("Close tab")
        gui.wait_for(lambda: len(h.workspace(path, workspace)["panes"]) == 1, "empty split collapsed")
        assert h.workspace(path, workspace)["panes"][0]["id"] == pane and gui.alive(original_pid)
        h.pass_check("closing the only tab of a split collapses only that pane")
        h.rpc(path, "surface_focus", pane=pane, surface=original)
        h.send(path, pane, "kill -HUP $$")
        gui.wait_for(lambda: find("This tab's process was terminated by SIGHUP"), "external HUP explanation")
        click("Close workspace")
        gui.wait_for(lambda: all(ws["id"] != workspace for ws in h.tree(path)), "last workspace removed")
        assert not find("Keep tab")
        h.pass_check("last tab offers Close workspace and removes it with one confirmation")
        h.close_window(process)
        for mode, bus in (("off", real_bus), ("auto", private_bus)):
            options["process_isolation"] = mode
            (config / "options.json").write_text(json.dumps(options))
            h.env["DBUS_SESSION_BUS_ADDRESS"] = bus
            process, path = h.window("isolation-fallback-" + mode)
            workspace = h.rpc(path, "workspace_create", name="fallback", root=str(h.root))["workspace_created"]["id"]
            h.rpc(path, "workspace_focus", workspace=workspace)
            pane = h.workspace(path, workspace)["panes"][0]["id"]
            original = tabs()[0]["id"]
            pid = shell_pid(original)
            assert Path(f"/proc/{pid}/oom_score_adj").read_text().strip() == "500"
            assert "flowmux-" + str(process.pid) not in Path(f"/proc/{pid}/cgroup").read_text()
            h.pass_check(f"{mode} without isolation starts promptly with oom_score_adj 500 fallback")
            h.close_window(process)
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
