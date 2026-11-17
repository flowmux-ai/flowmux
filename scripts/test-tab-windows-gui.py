#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise native tab tear-off, window-to-window drops and detached-window actions."""
import argparse
import importlib.util
import json
import os
import re
from pathlib import Path
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
        def windows():
            result = []
            for window in connection.screen().root.query_tree().children:
                try:
                    pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                    kind = window.get_full_property(connection.intern_atom("_NET_WM_WINDOW_TYPE"), X.AnyPropertyType)
                    if (pid is not None and int(pid.value[0]) == process.pid
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

        def application():
            desktop = Atspi.get_desktop(0)
            desktop.clear_cache()
            return next((desktop.get_child_at_index(i) for i in range(desktop.get_child_count())
                         if desktop.get_child_at_index(i).get_process_id() == process.pid), None)

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

        def drag(window, name, x, y):
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
        cancel = fixture.wait_for(lambda: named("Cancel"), "rename cancel button")
        assert cancel.get_action_iface().do_action(0)
        fixture.wait_for(lambda: rename_heading() is None, "rename dialog closed")
        h.pass_check("tear-off has no phantom tab and double-click rename stays in its own window")
        keys("Control_L", "Shift_L", "t")
        added = fixture.wait_for(lambda: next((tab for tab in location()[1]["tabs"] if tab["id"] != surface), None), "second detached tab")
        h.send(socket, detached_pane["id"], "printf '\033]2;ADDED_TAB\007'")
        fixture.wait_for(lambda: named("ADDED_TAB"), "second detached tab visible")
        click(detached, "DRAG_ME")
        fixture.wait_for(lambda: any(tab["id"] == surface and tab["active"] for tab in location()[1]["tabs"]), "moved tab selectable")
        click(detached, "ADDED_TAB")
        fixture.wait_for(lambda: any(tab["id"] == added["id"] and tab["active"] for tab in location()[1]["tabs"]), "new tab selectable")
        h.rpc(socket, "surface_close", pane=detached_pane["id"], surface=added["id"])
        time.sleep(.3)
        h.pass_check("both the moved tab and a newly added tab can be selected")


        h.close_window(process)
        print("TAB_WINDOWS_GUI_OK", flush=True)
    finally:
        connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for name in ("gui", "cli"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = fixture.Harness(args)
    try:
        exercise(h)
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
