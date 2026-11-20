#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise native tab tear-off, window-to-window drops and detached-window actions."""
import argparse
import importlib.util
import json
import os
import re
from pathlib import Path
import subprocess
import sys
import time

from Xlib import X, XK, display, error
from Xlib.ext import xtest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("fixture", Path(__file__).with_name("test-ssh-workspace-gui.py"))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def exercise(h):
    h.env["GTK_A11Y"] = "atspi"
    h.env.pop("AT_SPI_BUS_ADDRESS", None)
    config = h.root / "config/flowmux"
    config.mkdir()
    (config / "options.json").write_text(json.dumps({"default_shell": "/bin/sh", "usage_bar_enabled": False}))
    h.start_display()
    os.environ.update(DISPLAY=h.env["DISPLAY"], DBUS_SESSION_BUS_ADDRESS=h.env["DBUS_SESSION_BUS_ADDRESS"])
    os.environ.pop("AT_SPI_BUS_ADDRESS", None)
    import gi
    gi.require_version("Atspi", "2.0")
    from gi.repository import Atspi
    Atspi.set_timeout(3000, 3000)
    process, socket = h.window("tab-windows")
    connection = display.Display(h.env["DISPLAY"])
    try:
        def windows(pid=None):
            wanted_pid = process.pid if pid is None else pid
            result = []
            for window in connection.screen().root.query_tree().children:
                try:
                    pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                    kind = window.get_full_property(connection.intern_atom("_NET_WM_WINDOW_TYPE"), X.AnyPropertyType)
                    if (pid is not None and int(pid.value[0]) == wanted_pid
                            and window.get_attributes().map_state == X.IsViewable
                            and kind is not None and connection.intern_atom("_NET_WM_WINDOW_TYPE_NORMAL") in kind.value):
                        result.append(window)
                except error.BadWindow:
                    pass  # A drag icon can disappear between the tree and property reads.
            return result

        main = fixture.wait_for(lambda: next(iter(windows()), None), "main window")
        main.configure(x=0, y=0, width=750, height=800)
        connection.sync()
        workspace = h.rpc(socket, "workspace_create", name="Main", root=str(h.root))["workspace_created"]["id"]
        pane = h.workspace(socket, workspace)["panes"][0]["id"]
        surface = h.rpc(socket, "surface_create", workspace=workspace, cwd=str(h.root))["surface_created"]["id"]
        h.rpc(socket, "surface_focus", pane=pane, surface=surface)
        h.send(socket, pane, "printf '\033]2;DRAG_ME\007'; printf 'SHELL_PID:%s\\n' \"$$\"")
        original_pid = fixture.wait_for(lambda: re.search(r"SHELL_PID:(\d+)", h.screen(socket, pane)), "original shell output").group(1)

        def walk(node):
            node.clear_cache()
            yield node
            for index in range(node.get_child_count()):
                child = node.get_child_at_index(index)
                if child:
                    yield from walk(child)

        def application(pid=None):
            desktop = Atspi.get_desktop(0)
            desktop.clear_cache()
            return next((desktop.get_child_at_index(i) for i in range(desktop.get_child_count())
                         if desktop.get_child_at_index(i).get_process_id() == (process.pid if pid is None else pid)), None)

        def named(name):
            app = application()
            return next((node for node in walk(app) if node.get_name() == name
                         and node.get_role() == Atspi.Role.PUSH_BUTTON
                         and node.get_state_set().contains(Atspi.StateType.SHOWING)), None) if app else None

        def point(window, name):
            node = fixture.wait_for(lambda: named(name), name)
            bounds = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            return connection.screen().root.translate_coords(window, bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)

        def click(window, name, count=1):
            window.set_input_focus(X.RevertToParent, X.CurrentTime)
            origin = point(window, name)
            xtest.fake_input(connection, X.MotionNotify, x=origin.x, y=origin.y)
            for _ in range(count):
                xtest.fake_input(connection, X.ButtonPress, 1)
                xtest.fake_input(connection, X.ButtonRelease, 1)
                connection.sync()
                time.sleep(.08)

        def drag(window, name, x, y, cancel=False):
            window.set_input_focus(X.RevertToParent, X.CurrentTime)
            origin = point(window, name)
            h.log("drag", origin=[origin.x, origin.y], target=[x, y], title=window.get_wm_name())
            xtest.fake_input(connection, X.MotionNotify, x=origin.x, y=origin.y)
            xtest.fake_input(connection, X.ButtonPress, 1)
            connection.sync()
            # Cross the drag threshold inside a narrow tab before traversing other controls.
            xtest.fake_input(connection, X.MotionNotify, x=origin.x + (10 if x > origin.x else -10), y=origin.y)
            connection.sync()
            time.sleep(.05)
            for step in range(1, 21):
                xtest.fake_input(connection, X.MotionNotify, x=round(origin.x + (x-origin.x)*step/20),
                                 y=round(origin.y + (y-origin.y)*step/20))
                connection.sync()
                time.sleep(.03)
            if cancel:
                code = connection.keysym_to_keycode(XK.string_to_keysym("Escape"))
                xtest.fake_input(connection, X.KeyPress, code)
                xtest.fake_input(connection, X.KeyRelease, code)
                connection.sync()
                time.sleep(.2)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            connection.sync()

        def location():
            return next((ws, leaf) for ws in h.tree(socket) for leaf in ws["panes"]
                        if any(tab["id"] == surface for tab in leaf["tabs"]))

        drag(main, "DRAG_ME", 1550, 950)
        detached_ws, detached_pane = fixture.wait_for(lambda: location() if location()[0]["id"] != workspace else None, "torn-off tab registered")
        detached = fixture.wait_for(lambda: next((w for w in windows() if w.id != main.id), None), "torn-off window")
        detached.configure(x=800, y=0, width=750, height=800)
        detached.set_input_focus(X.RevertToParent, X.CurrentTime)
        connection.sync()
        detached_ws, detached_pane = location()
        assert detached_ws["id"] != workspace
        h.send(socket, detached_pane["id"], "printf 'AFTER_%s\\n' TEAROFF")
        fixture.wait_for(lambda: "AFTER_TEAROFF" in h.screen(socket, detached_pane["id"]), "same shell after tear-off")
        h.pass_check("native tear-off retains the surface ID and running shell")
        if h.args.managed:
            host_spec = importlib.util.spec_from_file_location(
                "host_fixture", Path(__file__).with_name("test-agent-host-gui.py"))
            host_fixture = importlib.util.module_from_spec(host_spec)
            host_spec.loader.exec_module(host_fixture)
            host_socket = h.root / "run/flowmux-agent-host.sock"
            with (h.root / "host.log").open("w") as log:
                h.spawn([h.args.host, "--agent-host", "--socket", str(host_socket)], stdout=log, stderr=log)
            fixture.wait_for(host_socket.exists, "private Host")
            control = host_fixture.HostClient(host_socket, "relay")
            try:
                launched = subprocess.run(
                    [h.args.cli, "--socket", str(socket), "--json", "agent", "run",
                     "--pane", detached_pane["id"], "--cwd", str(h.root), "--", "/bin/sh", "-c",
                     "printf 'MANAGED_DETACHED_READY\\n'; read answer"],
                    env=h.env, text=True, capture_output=True, timeout=30)
                assert launched.returncode == 0, (launched.stdout, launched.stderr)
                result = json.loads(launched.stdout)
                agent = result["agent"]
                managed_surface = result["view"]["surface_created"]["id"]

                def host_agent():
                    return next(item for item in control.call("list")["agents"]
                                if item["instance"] == agent["instance"])

                def agent_rows():
                    app = application()
                    return [node for node in walk(app)
                            if node.get_role() == Atspi.Role.PUSH_BUTTON
                            and "Open the same running agent" in (node.get_description() or "")
                            and node.get_state_set().contains(Atspi.StateType.SHOWING)] if app else []

                fixture.wait_for(lambda: len(agent_rows()) == 2, "managed row in both window sidebars")
                fixture.wait_for(lambda: (host_agent().get("view") or {}).get("visible"),
                                 "managed tab in detached window is Visible")
                h.rpc(socket, "surface_focus", pane=detached_pane["id"], surface=surface)
                fixture.wait_for(lambda: (view := host_agent().get("view")) and not view["visible"],
                                 "inactive detached tab is Open")
                h.rpc(socket, "surface_focus", pane=detached_pane["id"], surface=managed_surface)
                fixture.wait_for(lambda: (host_agent().get("view") or {}).get("visible"),
                                 "reactivated detached tab is Visible")
                assert host_agent()["pid"] == agent["pid"], "tab selection must keep the execution"
                h.log("managed-detached-view", agent=host_agent(), rows=len(agent_rows()))
                h.pass_check("both window sidebars list the same Agent and detached tab visibility follows selection")

                def frame(node):
                    while (parent := node.get_parent()) and parent.get_role() != Atspi.Role.APPLICATION:
                        node = parent
                    return node

                def shown(root, role, name):
                    return next((node for node in walk(root) if node.get_role() == role
                                 and node.get_name() == name
                                 and node.get_state_set().contains(Atspi.StateType.SHOWING)), None)

                owner = next(frame(node) for node in agent_rows()
                             if frame(node).get_name() == detached.get_wm_name())
                stop = fixture.wait_for(lambda: shown(owner, Atspi.Role.PUSH_BUTTON, "Terminate agent"),
                                        "detached Agent stop button")
                assert stop.get_state_set().contains(Atspi.StateType.SENSITIVE)
                assert host_agent()["state"] == "running" and host_agent()["pid"] == agent["pid"]
                h.pass_check("the detached window's Agents row offers the stop button for the same running execution")
                h.rpc(socket, "surface_focus", pane=detached_pane["id"], surface=surface)
                main.set_input_focus(X.RevertToParent, X.CurrentTime)
                connection.sync()
                handle = next(node for node in agent_rows()
                              if frame(node).get_name() == main.get_wm_name())
                button = next(node for node in walk(handle.get_parent())
                              if node.get_role() == Atspi.Role.PUSH_BUTTON)
                assert button.get_action_iface().do_action(0)
                # Bare Xvfb has no window manager to accept activation/focus requests.
                fixture.wait_for(lambda: (host_agent().get("view") or {}).get("visible")
                                 and windows()[-1].id == detached.id,
                                 "Agent row raises the detached window and its live tab")
                assert host_agent()["pid"] == agent["pid"]
                h.log("managed-detached-presentation", expected=detached.id,
                      topmost=windows()[-1].id, input_focus=connection.get_input_focus().focus.id)
                h.pass_check("Agent row opens the existing detached tab and raises its window above the main window")
            finally:
                control.close()
                control = host_fixture.HostClient(host_socket, "relay")
                try:
                    remaining = control.call("list")["agents"]
                    for item in remaining:
                        assert item["cwd"] == str(h.root), item
                        control.call("terminate", instance=item["instance"], epoch=item["epoch"], mode="terminate")
                    if remaining:
                        fixture.wait_for(lambda: not host_socket.exists(), "private Host reaps its execution", timeout=10)
                finally:
                    control.close()
            h.rpc(socket, "surface_close", pane=detached_pane["id"], surface=managed_surface)
            h.rpc(socket, "surface_focus", pane=detached_pane["id"], surface=surface)
        buttons = [node for node in walk(application()) if node.get_role() == Atspi.Role.PUSH_BUTTON
                   and node.get_state_set().contains(Atspi.StateType.SHOWING)
                   and node.get_name() in ("DRAG_ME", h.root.name)]
        assert len(buttons) == 2, "the two model tabs must render as exactly two selectable tabs"

        def keys(*names):
            detached.set_input_focus(X.RevertToParent, X.CurrentTime)
            codes = [connection.keysym_to_keycode(XK.string_to_keysym(name)) for name in names]
            for code in codes:
                xtest.fake_input(connection, X.KeyPress, code)
            for code in reversed(codes):
                xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()

        # Split with the installed keyboard action, then create a workspace in that window.
        keys("Control_L", "Shift_L", "Page_Up")
        fixture.wait_for(lambda: len(h.workspace(socket, detached_ws["id"])["panes"]) == 2, "detached split shortcut")
        keys("Control_L", "n")
        created = fixture.wait_for(lambda: next((ws for ws in h.tree(socket)
                                   if ws["id"] not in (workspace, detached_ws["id"])), None), "detached new workspace shortcut")
        fixture.wait_for(lambda: len(windows()) == 2, "two application windows")
        h.send(socket, created["panes"][0]["id"], "printf '\033]2;DETACHED_NEW_WS\007'")
        fixture.wait_for(lambda: "DETACHED_NEW_WS" in (detached.get_wm_name() or ""), "new workspace belongs to the detached window")
        h.pass_check("detached window supports split and new workspace shortcuts")
        h.rpc(socket, "workspace_focus", workspace=detached_ws["id"])
        h.rpc(socket, "surface_focus", pane=detached_pane["id"], surface=surface)
        # Give the split pane enough tab-bar space for native pointer input.
        detached.configure(width=1100)
        connection.sync()
        time.sleep(.4)
        # Drop on the main pane's upper-left body region (add as a tab).
        drag(detached, "DRAG_ME", 360, 150)
        fixture.wait_for(lambda: location()[0]["id"] == workspace, "native drop back into main window")
        restored_pane = location()[1]["id"]
        h.send(socket, restored_pane, "printf 'AFTER_%s\\n' RETURN; printf 'RETURN_PID:%s\\n' \"$$\"")
        fixture.wait_for(lambda: "AFTER_RETURN" in h.screen(socket, restored_pane), "shell responds after return")
        fixture.wait_for(lambda: "RETURN_PID:" + original_pid in h.screen(socket, restored_pane), "same shell PID after both moves")
        assert len(windows()) == 2, "the other detached workspaces must remain open"
        assert created["id"] in {ws["id"] for ws in h.tree(socket)}
        h.pass_check("native cross-window drop moves the live tab back without closing sibling workspaces")
        # Leave only the moved tab in the main window, tear it off and drop onto the empty window.
        for tab in h.workspace(socket, workspace)["panes"][0]["tabs"]:
            if tab["id"] != surface:
                h.rpc(socket, "surface_close", pane=restored_pane, surface=tab["id"])
        prior_windows = {window.id for window in windows()}
        # Keep the previous window out of this tear-off path so it cannot accept a hover/drop.
        detached.configure(x=2000, y=0)
        main.configure(y=150)
        connection.sync()
        time.sleep(.3)
        drag(main, "DRAG_ME", 1550, 10)
        fixture.wait_for(lambda: location()[0]["id"] != workspace, "last tab torn off")
        last = fixture.wait_for(lambda: next((w for w in windows() if w.id not in prior_windows), None), "last tab's new window")
        last.configure(x=800, y=0, width=750, height=800)
        connection.sync()
        time.sleep(.4)
        drag(last, "DRAG_ME", 360, 350)
        fixture.wait_for(lambda: location()[0]["id"] not in (detached_ws["id"], created["id"])
                         and location()[1]["id"] != detached_pane["id"] and len(windows()) == 2,
                         "drop into empty main window closes the emptied source window")
        restored_pane = location()[1]["id"]
        h.send(socket, restored_pane, "printf 'EMPTY_RETURN_PID:%s\\n' \"$$\"")
        fixture.wait_for(lambda: "EMPTY_RETURN_PID:" + original_pid in h.screen(socket, restored_pane), "same shell after empty-window return")
        h.pass_check("an empty main window accepts its last tab back and closes the emptied window")
        # Test rename and selection in a fresh detached window after the movement scenarios.
        prior_windows = {window.id for window in windows()}
        drag(main, "DRAG_ME", 1550, 10)
        fixture.wait_for(lambda: len(windows()) == 3, "window for rename and selection")
        detached = next(window for window in windows() if window.id not in prior_windows)
        detached.configure(x=800, y=0, width=750, height=800)
        connection.sync()
        detached_ws, detached_pane = location()
        # A real double-click must open the rename dialog inside the detached window.
        click(detached, "DRAG_ME", count=2)

        def rename_heading():
            return next((node for node in walk(application()) if node.get_name() == "Rename Pane Tab"
                         and node.get_state_set().contains(Atspi.StateType.SHOWING)), None)

        heading = fixture.wait_for(rename_heading, "double-click rename dialog")
        owner = heading
        while (parent := owner.get_parent()) and parent.get_role() != Atspi.Role.APPLICATION:
            owner = parent
        assert "DRAG_ME" in owner.get_name(), "rename dialog must belong to the detached window"
        entry = next(node for node in walk(owner) if node.get_role() == Atspi.Role.TEXT
                     and node.get_state_set().contains(Atspi.StateType.SHOWING)
                     and node.get_state_set().contains(Atspi.StateType.EDITABLE))
        assert entry.get_editable_text_iface().set_text_contents("RENAMED_TAB")
        click(detached, "OK")
        fixture.wait_for(lambda: rename_heading() is None, "rename dialog closed")
        fixture.wait_for(lambda: any(tab["id"] == surface and tab["title"] == "RENAMED_TAB" for tab in location()[1]["tabs"]), "tab renamed")
        time.sleep(.3)
        # No right-click or context-menu detour may be needed to restore dragging.
        renamed_workspace = detached_ws["id"]
        drag(detached, "RENAMED_TAB", 360, 350)
        fixture.wait_for(lambda: location()[0]["id"] != renamed_workspace and len(windows()) == 2,
                         "renamed tab drags immediately without a right-click")
        h.pass_check("double-click rename is followed immediately by a successful native drag")
        prior_windows = {window.id for window in windows()}
        drag(main, "RENAMED_TAB", 1550, 10)
        fixture.wait_for(lambda: len(windows()) == 3, "renamed tab detached again")
        detached = next(window for window in windows() if window.id not in prior_windows)
        detached.configure(x=800, y=0, width=750, height=800)
        connection.sync()
        detached_ws, detached_pane = location()
        other_names = {ws["id"]: ws["name"] for ws in h.tree(socket) if ws["id"] != detached_ws["id"]}
        keys("Control_L", "Alt_L", "r")
        heading = fixture.wait_for(lambda: next((node for node in walk(application()) if node.get_name() == "Rename Tab"
                                                and node.get_state_set().contains(Atspi.StateType.SHOWING)), None), "workspace rename shortcut")
        owner = heading
        while (parent := owner.get_parent()) and parent.get_role() != Atspi.Role.APPLICATION:
            owner = parent
        assert "RENAMED_TAB" in owner.get_name(), "Ctrl+Alt+R must target the invoking window's workspace"
        entry = next(node for node in walk(owner) if node.get_role() == Atspi.Role.TEXT
                     and node.get_state_set().contains(Atspi.StateType.SHOWING)
                     and node.get_state_set().contains(Atspi.StateType.EDITABLE))
        assert entry.get_editable_text_iface().set_text_contents("RENAMED_WORKSPACE")
        click(detached, "OK")
        fixture.wait_for(lambda: h.workspace(socket, detached_ws["id"])["name"] == "RENAMED_WORKSPACE",
                         "shortcut renames the visible workspace")
        assert all(ws["name"] == other_names[ws["id"]] for ws in h.tree(socket) if ws["id"] in other_names)
        time.sleep(.3)
        h.pass_check("Ctrl+Alt+R renames the visible workspace in the invoking detached window")
        keys("Control_L", "Shift_L", "t")
        added = fixture.wait_for(lambda: next((tab for tab in location()[1]["tabs"] if tab["id"] != surface), None), "second detached tab")
        h.send(socket, detached_pane["id"], "printf '\033]2;ADDED_TAB\007'")
        fixture.wait_for(lambda: named("ADDED_TAB"), "second detached tab visible")
        click(detached, "RENAMED_TAB")
        fixture.wait_for(lambda: any(tab["id"] == surface and tab["active"] for tab in location()[1]["tabs"]), "moved tab selectable")
        click(detached, "ADDED_TAB")
        fixture.wait_for(lambda: any(tab["id"] == added["id"] and tab["active"] for tab in location()[1]["tabs"]), "new tab selectable")
        h.rpc(socket, "surface_close", pane=detached_pane["id"], surface=added["id"])
        time.sleep(.3)
        h.pass_check("both the moved tab and a newly added tab can be selected")


        # Both detached hosts remain alive while the tab crosses in each direction.
        keys("Control_L", "Shift_L", "t")
        fixture.wait_for(lambda: any(tab["id"] != surface and tab["active"] for tab in location()[1]["tabs"]), "source anchor created and active")
        h.send(socket, detached_pane["id"], "printf '\\033]2;SOURCE_ANCHOR\\007'")
        fixture.wait_for(lambda: named("SOURCE_ANCHOR"), "source anchor tab")
        peer = next(window for window in windows() if window.id not in (main.id, detached.id))
        main.configure(x=2000)
        detached.configure(x=0, y=0, width=750, height=800)
        peer.configure(x=800, y=0, width=750, height=800)
        h.rpc(socket, "workspace_focus", workspace=created["id"])
        connection.sync()
        time.sleep(.4)
        drag(detached, "RENAMED_TAB", 1160, 150)
        fixture.wait_for(lambda: location()[0]["id"] == created["id"], "detached to detached")
        drag(peer, "RENAMED_TAB", 360, 150)
        fixture.wait_for(lambda: location()[0]["id"] == detached_ws["id"], "detached to detached return")
        assert len(windows()) == 3
        # Native split targets in another window, then the reverse direction.
        drag(detached, "RENAMED_TAB", 1500, 200)
        fixture.wait_for(lambda: location()[0]["id"] == created["id"] and len(h.workspace(socket, created["id"])["panes"]) == 2,
                         "cross-window right split")
        drag(peer, "RENAMED_TAB", 360, 730)
        fixture.wait_for(lambda: location()[0]["id"] == detached_ws["id"] and len(h.workspace(socket, detached_ws["id"])["panes"]) == 2,
                         "cross-window bottom split")
        h.pass_check("detached windows exchange the live tab through body and both split targets")
        # Escape outside any target must not detach or commit a split preview.
        before = (location()[0]["id"], location()[1]["id"], {w.id for w in windows()})
        drag(detached, "RENAMED_TAB", 1550, 950, cancel=True)
        time.sleep(.5)
        assert (location()[0]["id"], location()[1]["id"], {w.id for w in windows()}) == before
        h.pass_check("Escape cancels an outside drag without opening a window")
        main.configure(x=0, y=0)
        detached.configure(x=800, y=0, width=1100)
        peer.configure(x=2000)
        connection.sync()
        time.sleep(.4)
        # Reproduce: a detached tab, a new workspace requested in the main window,
        # then a native drop directly on that new workspace's side-panel row.
        prior = {ws["id"] for ws in h.tree(socket)}
        click(main, "New workspace")
        target = fixture.wait_for(lambda: next((ws for ws in h.tree(socket) if ws["id"] not in prior), None), "main workspace created")
        h.send(socket, target["panes"][0]["id"], "printf '\\033]2;ROW_TARGET\\007'")
        def row_point():
            # A workspace and its active pane tab can have the same label.
            for node in walk(application()):
                if (node.get_name() != "ROW_TARGET" or node.get_role() != Atspi.Role.LABEL
                        or not node.get_state_set().contains(Atspi.StateType.SHOWING)):
                    continue
                parent = node.get_parent()
                while parent and parent.get_role() not in (Atspi.Role.LIST_ITEM, Atspi.Role.APPLICATION):
                    parent = parent.get_parent()
                if parent and parent.get_role() == Atspi.Role.LIST_ITEM:
                    bounds = parent.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                    return connection.screen().root.translate_coords(
                        main, bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
            return None

        destination = fixture.wait_for(row_point, "new workspace row")
        h.rpc(socket, "workspace_focus", workspace=detached_ws["id"])
        detached.configure(width=1100)
        connection.sync()
        time.sleep(.4)
        count_before = len(windows())
        # Hover a non-active workspace row and cancel: no switch or tear-off.
        prior = {ws["id"] for ws in h.tree(socket)}
        click(main, "New workspace")
        visible = fixture.wait_for(lambda: next((ws for ws in h.tree(socket) if ws["id"] not in prior), None), "other main workspace")
        h.send(socket, visible["panes"][0]["id"], "printf '\\033]2;KEEP_VISIBLE\\007'")
        fixture.wait_for(lambda: "KEEP_VISIBLE" in (main.get_wm_name() or ""), "main workspace visible")
        h.rpc(socket, "workspace_focus", workspace=detached_ws["id"])
        destination = fixture.wait_for(row_point, "workspace row after creation")
        h.log("workspace-row-target", x=destination.x, y=destination.y)
        drag(detached, "RENAMED_TAB", destination.x, destination.y, cancel=True)
        time.sleep(.5)
        assert location()[0]["id"] == detached_ws["id"]
        assert "KEEP_VISIBLE" in (main.get_wm_name() or ""), "hover must not change the visible workspace"
        assert len(windows()) == count_before
        h.pass_check("workspace-row hover followed by Escape preserves focus and source")
        destination = fixture.wait_for(row_point, "workspace row after cancel")
        drag(detached, "RENAMED_TAB", destination.x, destination.y)
        fixture.wait_for(lambda: location()[0]["id"] == target["id"], "drop reaches explicitly named new workspace")
        assert len(windows()) == count_before, "workspace-row drop must not open another window"
        h.pass_check("detached tab drops into a newly created workspace in the other window")
        # Native tab-bar insertion on both halves, including an in-pane reorder.
        anchor_point = point(detached, "SOURCE_ANCHOR")
        drag(main, "RENAMED_TAB", anchor_point.x - 25, anchor_point.y)
        fixture.wait_for(lambda: location()[0]["id"] == detached_ws["id"], "main to detached tab bar")
        fixture.wait_for(lambda: location()[1]["tabs"][0]["id"] == surface, "insert before target tab")
        anchor_point = point(detached, "SOURCE_ANCHOR")
        drag(detached, "RENAMED_TAB", anchor_point.x + 25, anchor_point.y)
        fixture.wait_for(lambda: location()[1]["tabs"][-1]["id"] == surface, "insert after target tab")
        # Exercise the actual context-menu action after these drags.
        origin = point(detached, "RENAMED_TAB")
        xtest.fake_input(connection, X.MotionNotify, x=origin.x, y=origin.y)
        xtest.fake_input(connection, X.ButtonPress, 3)
        xtest.fake_input(connection, X.ButtonRelease, 3)
        connection.sync()
        item = fixture.wait_for(lambda: named("Move to main window"), "return menu item")
        assert item.get_action_iface().do_action(0)
        fixture.wait_for(lambda: location()[0]["id"] == target["id"], "context menu returns tab to main window")
        h.pass_check("tab-bar before/after insertion and Move to main window preserve the live tab")
        # The numbered Move submenu must retain workspaces hidden from the main side panel.
        detached.configure(x=2000)
        peer.configure(x=800, y=0, width=750, height=800)
        connection.sync()
        origin = point(main, "RENAMED_TAB")
        xtest.fake_input(connection, X.MotionNotify, x=origin.x, y=origin.y)
        xtest.fake_input(connection, X.ButtonPress, 3)
        xtest.fake_input(connection, X.ButtonRelease, 3)
        connection.sync()
        move = fixture.wait_for(lambda: next((node for node in walk(application()) if node.get_name() == "Move"
                                              and node.get_action_iface() is not None
                                              and node.get_action_iface().get_n_actions() > 0
                                              and node.get_state_set().contains(Atspi.StateType.SHOWING)), None), "Move submenu")
        assert move.get_action_iface().do_action(0)
        current_tree = h.tree(socket)
        number = next(i + 1 for i, ws in enumerate(current_tree) if ws["id"] == created["id"])
        title = next(ws["name"] for ws in current_tree if ws["id"] == created["id"])
        item = fixture.wait_for(lambda: named(f"{number}. {title}"), "other window workspace in Move menu")
        assert item.get_action_iface().do_action(0)
        fixture.wait_for(lambda: location()[0]["id"] == created["id"], "Move submenu reaches detached workspace")
        origin = point(peer, "RENAMED_TAB")
        xtest.fake_input(connection, X.MotionNotify, x=origin.x, y=origin.y)
        xtest.fake_input(connection, X.ButtonPress, 3)
        xtest.fake_input(connection, X.ButtonRelease, 3)
        connection.sync()
        item = fixture.wait_for(lambda: named("Move to main window"), "return from Move submenu")
        assert item.get_action_iface().do_action(0)
        fixture.wait_for(lambda: location()[0]["id"] == target["id"], "menu round trip reaches main workspace")
        h.pass_check("Move submenu lists and reaches a workspace owned by another window")
        peer.configure(x=2000)
        connection.sync()
        # Separately launched processes cannot reparent a live GTK/backend object.
        # They must reject the drop without copying, deleting or detaching the source.
        foreign, foreign_socket = h.window("foreign-tabs")
        foreign_window = fixture.wait_for(lambda: next(iter(windows(foreign.pid)), None), "foreign window")
        foreign_window.configure(x=800, y=0, width=750, height=800)
        connection.sync()
        foreign_ws = h.rpc(foreign_socket, "workspace_create", name="Foreign", root=str(h.root))["workspace_created"]["id"]
        foreign_before = h.tree(foreign_socket)
        source_before = (location()[0]["id"], location()[1]["id"], {w.id for w in windows()})
        for destination in [(1160, 150), (850, 100)]:
            drag(main, "RENAMED_TAB", *destination)
            time.sleep(.7)
            assert h.tree(foreign_socket) == foreign_before, "foreign drop must not clone a surface"
            assert (location()[0]["id"], location()[1]["id"], {w.id for w in windows()}) == source_before
        h.send(socket, location()[1]["id"], "printf 'AUDIT_PID:%s\\n' \"$$\"")
        fixture.wait_for(lambda: "AUDIT_PID:" + original_pid in h.screen(socket, location()[1]["id"]), "original PID after all drag routes")
        h.pass_check("foreign-process body/sidebar drops preserve both models and the original shell")
        h.close_window(foreign)
        h.close_window(process)
        print("TAB_WINDOWS_GUI_OK", flush=True)
    finally:
        connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--host", default="target/debug/flowmux-daemon")
    parser.add_argument("--managed", action="store_true", help="also verify Agents in detached windows")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for name in ["gui", "cli"] + (["host"] if args.managed else []):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = fixture.Harness(args)
    try:
        exercise(h)
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
