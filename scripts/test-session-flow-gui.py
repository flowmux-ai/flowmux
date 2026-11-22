#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check Fork session with Session Flow hidden in a disposable Linux GUI."""
from agent_host_protocol import SOCKET_NAME

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
spec = importlib.util.spec_from_file_location(
    "host_fixture", Path(__file__).with_name("test-agent-host-gui.py"))
host_fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host_fixture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--host", default="target/debug/flowmux-daemon")
    parser.add_argument("--tab-fallback", action="store_true", help="refuse Host startup and fork into an ordinary tab")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for name in ("gui", "cli", "host"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = gui.Harness(args)
    connection = None
    host_socket = h.root / "run" / SOCKET_NAME
    try:
        h.env.update(GTK_A11Y="atspi", CLAUDE_CONFIG_DIR=str(h.root / "claude-home"))
        h.env.pop("AT_SPI_BUS_ADDRESS", None)
        history = Path(h.env["CLAUDE_CONFIG_DIR"]) / "projects/fixture"
        history.mkdir(parents=True)
        shell = h.root / "clean-shell"
        shell.write_text('#!/bin/sh\nif [ "$1" = "-lc" ] || [ "$1" = "-lic" ]; then exec /bin/sh -c "$2"; fi\n'
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
    if (argc > 1) {
        FILE *file = fopen("fork-info.tmp", "w"); if (!file) return 2;
        fprintf(file, "%d\n", getpid());
        for (int i = 1; i < argc; i++) fprintf(file, "%s\n", argv[i]);
        fclose(file); if (rename("fork-info.tmp", "fork-info")) return 2;
    }
    printf("fixture-pid:%d\n", getpid());
    for (int i = 1; i < argc; i++) printf("fixture-arg:%s\n", argv[i]);
    puts("❯ ");
    fflush(stdout);
    while (getchar() != EOF) {}
    return 0;
}
''')
        executable = h.root / "claude"
        subprocess.run(["cc", str(source), "-o", str(executable)], check=True, timeout=30)
        h.env["PATH"] = str(h.root) + os.pathsep + h.env["PATH"]
        if args.tab_fallback:
            # Refuse the user service manager without the Flatpak side effects of FLATPAK_ID.
            refuse = h.root / "refuse-service"
            refuse.mkdir()
            (refuse / "systemd-run").write_text(f'#!/bin/sh\ntouch {h.root}/service-refused\nexit 1\n')
            (refuse / "systemd-run").chmod(0o700)
            h.env["PATH"] = str(refuse) + os.pathsep + h.env["PATH"]
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

        def find(role, name=None, prefix=None, description=None):
            desktop = Atspi.get_desktop(0)
            desktop.clear_cache()
            pending = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
            pending = [node for node in pending if node and node.get_process_id() == process.pid]
            while pending:
                node = pending.pop()
                if node is None:
                    continue
                node.clear_cache()
                if ((role is None or node.get_role() == role) and (name is None or node.get_name() == name)
                        and (prefix is None or node.get_name().startswith(prefix))
                        and (description is None or node.get_description() == description)
                        and node.get_state_set().contains(Atspi.StateType.SHOWING)):
                    return node
                pending.extend(node.get_child_at_index(i) for i in range(node.get_child_count()))

        def menu_enabled(activate=False):
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
            if activate and enabled:
                assert button.get_action_iface().do_action(0)
            else:
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

        def transcript(session, content="Fork fixture"):
            (history / f"{session}.jsonl").write_text(json.dumps(dict(
                type="user", sessionId=session, cwd=str(h.root),
                message=dict(role="user", content=content))) + "\n")

        def imported(session):
            with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as db:
                return db.execute("SELECT COUNT(*) FROM sessions WHERE key LIKE ?",
                                  (f"%{session}%",)).fetchone()[0] == 1

        def keypress(name):
            code = connection.keysym_to_keycode(XK.string_to_keysym(name))
            xtest.fake_input(connection, X.KeyPress, code)
            xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()

        def click(node):
            bounds = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            point = connection.screen().root.translate_coords(
                window, bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
            window.set_input_focus(X.RevertToParent, X.CurrentTime)
            xtest.fake_input(connection, X.MotionNotify, x=point.x, y=point.y)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            connection.sync()

        def fork_info():
            path = h.root / "fork-info"
            return path.read_text().splitlines() if path.exists() else None

        def tab_fallback(source):
            # Skip the long history checks; the subject is the ordinary-tab path.
            notice = "Session opened in an ordinary tab of this window"
            assert menu_enabled(activate=True)
            gui.wait_for(lambda: find(Atspi.Role.LABEL, prefix=notice), "tab-bound fork notice", timeout=20)
            child = gui.wait_for(lambda: next((p for p in h.workspace(socket, workspace)["panes"]
                                              if p["id"] != pane), None), "fork pane")
            info = gui.wait_for(fork_info, "fork agent")
            assert int(info[0]) != pid and info[1:] == [
                "--resume", source, "--fork-session", "--allow-dangerously-skip-permissions",
                "--permission-mode", "bypassPermissions"], info
            gui.wait_for(lambda: f"fixture-pid:{info[0]}" in h.screen(socket, child["id"]), "fork in ordinary tab")
            assert gui.alive(pid) and (h.root / "service-refused").exists() and not host_socket.exists()
            h.pass_check("Refused Host startup forks once into an ordinary split tab")
            # The tab's hook identity completes the fork; the ended child then resumes from Session Flow.
            child_id, child_surface = str(uuid.uuid4()), child["tabs"][0]["id"]
            h.rpc(socket, "agent_activity_update", pane=child["id"], surface=child_surface, agent="claude",
                  pid=int(info[0]), source="flowmux:hook", session_id=child_id, seq=time.time_ns(),
                  activity="idle", custom_status="Ready")

            def completed():
                with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as db:
                    return db.execute("SELECT COUNT(*) FROM operations WHERE state='complete'").fetchone()[0]
            gui.wait_for(lambda: completed() == 1, "tab-bound fork identity completes")
            # The resume must show its own notice, not this fork's.
            gui.wait_for(lambda: find(Atspi.Role.LABEL, prefix=notice) is None, "fork notice expired", timeout=15)
            transcript(child_id, "Fork child")
            (h.root / "fork-info").unlink()
            h.rpc(socket, "surface_close", pane=child["id"], surface=child_surface)
            gui.wait_for(lambda: len(h.workspace(socket, workspace)["panes"]) == 1, "fork tab closed")
            gui.wait_for(lambda: not gui.alive(int(info[0])), "fork process ended")
            h.rpc(socket, "surface_focus", pane=pane, surface=surface)
            button = gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Session Flow"), "Flow button")
            assert button.get_action_iface().do_action(0)
            fit = gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fit"), "opened Session Flow")
            # Refresh drops the ended tab's live binding before an explicit resume.
            assert find(Atspi.Role.PUSH_BUTTON, "↻").get_action_iface().do_action(0)
            gui.wait_for(lambda: imported(child_id), "child history imported")
            time.sleep(.5)
            graph = "Session Flow graph. Arrow keys select a session; Enter opens it."
            click(fit)
            gui.wait_for(lambda: fit.get_state_set().contains(Atspi.StateType.FOCUSED), "Flow toolbar focus")
            for description in ("Refresh native session history", "Close Session Flow", graph):
                keypress("Tab")
                gui.wait_for(lambda: (n := find(None, description=description))
                             and n.get_state_set().contains(Atspi.StateType.FOCUSED), "focus: " + description)
            for _ in range(4):
                previous = find(None, description=graph).get_name()
                keypress("Down")
                selected = gui.wait_for(lambda: (n := find(None, description=graph)) and n.get_name() != previous
                                        and n.get_name(), "Flow keyboard selection")
                h.log("flow-selection", selected=selected)
                if selected == "claude · Fork child · Select to resume in a right split":
                    break
            else:
                raise AssertionError(f"Cannot select the ended fork child: {selected}")
            keypress("Return")
            gui.wait_for(lambda: find(Atspi.Role.LABEL, prefix=notice), "tab-bound resume notice", timeout=20)
            info = gui.wait_for(fork_info, "resumed agent")
            assert info[1:] == ["--resume", child_id], info
            resumed = gui.wait_for(lambda: next((p for p in h.workspace(socket, workspace)["panes"]
                                                if p["id"] != pane), None), "resumed split")
            gui.wait_for(lambda: f"fixture-pid:{info[0]}" in h.screen(socket, resumed["id"]), "resume in ordinary tab")
            assert not host_socket.exists()
            h.pass_check("Refused Host startup resumes a Session Flow node once into an ordinary split tab")

        first, second = str(uuid.uuid4()), str(uuid.uuid4())
        transcript(first)
        report(first, start=True)
        gui.wait_for(lambda: imported(first), "new session imported with panel hidden", timeout=12)
        gui.wait_for(menu_enabled, "new idle session fork enabled", timeout=8)
        h.pass_check("new idle session enables Fork without opening Session Flow")
        if args.tab_fallback:
            tab_fallback(first)
            return
        for index in range(3):
            text = f"Progress update {index}"
            h.rpc(socket, "agent_activity_update", pane=pane, surface=surface, agent="claude",
                  pid=pid, source="flowmux:hook", session_id=first,
                  seq=time.time_ns(), activity="idle", custom_status=text)
            gui.wait_for(lambda: find(Atspi.Role.LABEL, prefix=text + " · "),
                         "updated activity row text")
        h.pass_check("activity row text updates without changing agent state")
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
        # An empty Host retires after ten seconds; start after the delayed-history check.
        with (h.root / "host.log").open("w") as log:
            host = h.spawn([args.host, "--agent-host", "--socket", str(host_socket)], stdout=log, stderr=log)
        gui.wait_for(host_socket.exists, "isolated Host")
        assert menu_enabled(activate=True)
        child = gui.wait_for(lambda: next((p for p in h.workspace(socket, workspace)["panes"]
                                          if p["id"] != pane), None), "fork pane")
        # The narrow managed view may wrap argv and scroll the initial PID offscreen.
        info = gui.wait_for(lambda: (h.root / "fork-info").read_text().splitlines()
                            if (h.root / "fork-info").exists() else None, "fork agent")
        assert int(info[0]) != pid
        gui.wait_for(lambda: "bypassPermissions" in h.screen(socket, child["id"]), "fork output in live view")
        arguments = info[1:]
        assert arguments == ["--resume", second, "--fork-session",
                             "--allow-dangerously-skip-permissions",
                             "--permission-mode", "bypassPermissions"], arguments
        assert gui.alive(pid), "fork stopped the source agent"
        assert f"fixture-pid:{pid}" in h.screen(socket, pane)
        h.pass_check("Fork launches a new YOLO agent in a split pane and preserves the source")
        button = gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Session Flow"), "Flow button")
        assert button.get_action_iface().do_action(0)
        gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fit"), "opened Session Flow")
        other = h.rpc(socket, "workspace_create", name="Closed Flow", root=str(h.root))["workspace_created"]["id"]
        h.rpc(socket, "workspace_focus", workspace=other)
        gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fit") is None, "new workspace hides Flow")
        h.rpc(socket, "workspace_focus", workspace=workspace)
        gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fit"), "saved open Flow restored")
        h.pass_check("workspace switching restores Flow visibility after hidden rendering was skipped")
        h.close_window(process)
    finally:
        if connection is not None:
            connection.close()
        try:
            if host_socket.exists():
                control = host_fixture.HostClient(host_socket, "relay")
                try:
                    for agent in control.call("list")["agents"]:
                        assert agent["cwd"] == str(h.root), agent
                        control.call("terminate", instance=agent["instance"], epoch=agent["epoch"], mode="terminate")
                    # The closed window's terminals end ten seconds after it.
                    gui.wait_for(lambda: not host_socket.exists(), "private Host reaps forked execution", timeout=40)
                    assert host.wait(timeout=5) == 0
                finally:
                    control.close()
        finally:
            h.cleanup()


if __name__ == "__main__":
    main()
