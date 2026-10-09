#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify AgentOffice in an isolated Linux GUI using AT-SPI and native keys.

Requires the shared GUI harness dependencies, Pillow and the Atspi GI binding.
Screenshots and event logs remain in the printed artifact directory.
"""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import sys
import time

from PIL import Image
from Xlib import X, XK, display
from Xlib.ext import xtest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "gui", Path(__file__).with_name("test-ssh-workspace-gui.py")
)
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
    h = gui.Harness(args)
    connection = None
    try:
        h.env["GTK_A11Y"] = "atspi"
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell)}))
        h.start_display()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        connection = display.Display(h.env["DISPLAY"])
        process, socket = h.window("agent-office")

        def nodes(root=None):
            if root is not None:
                yield root
                for i in range(root.get_child_count()):
                    yield from nodes(root.get_child_at_index(i))
                return
            desktop = Atspi.get_desktop(0)
            pending = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
            app = next((app for app in pending if app.get_process_id() == process.pid), None)
            pending = [app] if app else []
            while pending:
                node = pending.pop()
                if node is not None:
                    yield node
                    pending.extend(node.get_child_at_index(i) for i in range(node.get_child_count()))

        def find(text):
            return next((node for node in nodes() if node.get_name() == text), None)

        def office_nodes():
            root = find("AgentOffice map")
            return nodes(root) if root is not None else iter(())

        def room(name):
            return next((node for node in office_nodes()
                         if node.get_role() == Atspi.Role.PUSH_BUTTON
                         and node.get_name().startswith(name + " · ")), None)

        def native_window():
            atom = connection.intern_atom("_NET_WM_PID")
            for window in connection.screen().root.query_tree().children:
                pid = window.get_full_property(atom, X.AnyPropertyType)
                if pid is not None and int(pid.value[0]) == process.pid and window.get_geometry().width > 100:
                    return window
            raise AssertionError("No test application window")

        def click(node):
            assert node.get_action_iface().do_action(0), node.get_name()

        def screenshot(name):
            time.sleep(.3)
            root = connection.screen().root
            geometry = root.get_geometry()
            raw = root.get_image(0, 0, geometry.width, geometry.height, X.ZPixmap, 0xffffffff)
            Image.frombytes("RGB", (geometry.width, geometry.height), raw.data,
                            "raw", "BGRX").save(h.root / f"{name}.png")

        def packed_floor(name):
            screenshot(name)
            geometry = native_window().get_geometry()
            picture = Image.open(h.root / f"{name}.png")
            floor = picture.crop((4, 92, geometry.width - 4, geometry.height - 44))
            pixels = floor.width * floor.height
            empty = sum(count for count, color in floor.getcolors(pixels) if color == (17, 25, 35))
            assert empty / pixels < .03, f"Offices leave {empty / pixels:.1%} unused space"

        studio_root = h.root / "Product studio"
        quiet_root = h.root / "Quiet corner"
        studio_root.mkdir()
        quiet_root.mkdir()
        workspace = h.rpc(socket, "workspace_create", name="Product studio", root=str(studio_root))["workspace_created"]["id"]
        residents = []
        for index, name in enumerate(("codex", "claude", "gemini")):
            if index:
                tab = h.rpc(socket, "surface_create", workspace=workspace, cwd=str(studio_root))["surface_created"]
                pane, surface = tab["pane"], tab["id"]
            else:
                pane_data = h.workspace(socket, workspace)["panes"][0]
                pane, surface = pane_data["id"], pane_data["tabs"][0]["id"]
            executable = h.root / name
            shutil.copyfile(shutil.which("sleep"), executable)
            executable.chmod(0o755)
            h.send(socket, pane, f"exec {shlex.quote(str(executable))} 600")
            residents.append((name, pane, surface))
        quiet_id = h.rpc(socket, "workspace_create", name="Quiet corner", root=str(quiet_root))["workspace_created"]["id"]
        h.rpc(socket, "workspace_focus", workspace=workspace)

        def report(resident, status, detail=None):
            name, pane, surface = resident
            h.rpc(socket, "agent_activity_update", pane=pane, surface=surface, agent=name,
                  status=status, source="flowmux:hook", session_id=f"office-fixture-{name}", custom_status=detail)
            def observed():
                tab = next(tab for ws in h.tree(socket) for pane in ws["panes"]
                           for tab in pane["tabs"] if tab["id"] == surface)
                return tab.get("agent", {}).get("status") == status
            # A completion while covered is exposed as Done, rather than Idle.
            if status != "idle":
                gui.wait_for(observed, f"{name} {status}")

        for resident in residents:
            report(resident, "working")
        screenshot("footer-before")
        click(gui.wait_for(lambda: find("AgentOffice"), "footer button"))
        gui.wait_for(lambda: find("Back to workspace"), "office open")
        gui.wait_for(lambda: next((n for n in nodes() if "Working · at the desk" in n.get_name()), None), "working scene")
        assert room("Product studio") and room("Quiet corner")
        screenshot("working")
        report(residents[1], "blocked", "승인 대기 / Review changes")
        gui.wait_for(lambda: next((n for n in nodes() if "Needs you · waiting for input" in n.get_name()), None), "blocked scene")
        gemini = next(n for n in nodes() if n.get_role() == Atspi.Role.PUSH_BUTTON and n.get_name().startswith("gemini · "))
        before = gemini.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        report(residents[2], "idle")
        gui.wait_for(lambda: next((n for n in nodes() if "Done · taking a break" in n.get_name()), None), "completed scene")
        time.sleep(5)
        after = gemini.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        assert (before.x, before.y) != (after.x, after.y), "completed character walks to lounge"
        screenshot("mixed-status")
        h.pass_check("live workspaces, three agent providers, working / blocked / completed scenes")

        target = gui.wait_for(lambda: next((node for node in nodes()
            if node.get_role() == Atspi.Role.PUSH_BUTTON
            and node.get_name().startswith("claude · ")), None), "Claude character")
        click(target)
        gui.wait_for(lambda: find("Back to workspace") is None, "character returns to terminal")
        current = h.rpc(socket, "workspace_current")["workspace_current"]["id"]
        assert current == workspace
        tabs = h.workspace(socket, workspace)["panes"][0]["tabs"]
        assert next(tab for tab in tabs if tab["id"] == residents[1][2])["active"]
        click(gui.wait_for(lambda: find("AgentOffice"), "footer button after return"))
        gui.wait_for(lambda: find("Back to workspace"), "office reopened")
        key = connection.keysym_to_keycode(XK.string_to_keysym("Escape"))
        xtest.fake_input(connection, X.KeyPress, key)
        xtest.fake_input(connection, X.KeyRelease, key)
        connection.sync()
        gui.wait_for(lambda: find("Back to workspace") is None, "Escape returns to terminal")
        h.pass_check("character walks to lounge; navigation selects the matching tab; Escape closes office")
        def bounds(label):
            return find(label).get_component_iface().get_extents(Atspi.CoordType.WINDOW)

        settings, office, agents = (bounds(label) for label in ("Options", "AgentOffice", "Agents bar"))
        assert settings.x < office.x < agents.x
        assert office.x + office.width == agents.x
        screenshot("footer-left")
        xtest.fake_input(connection, X.MotionNotify, x=office.x + office.width + 10, y=settings.y + settings.height // 2)
        for _ in range(8):
            xtest.fake_input(connection, X.ButtonPress, 5)
            xtest.fake_input(connection, X.ButtonRelease, 5)
        connection.sync()
        time.sleep(.5)
        assert bounds("Options").x == settings.x, "settings stays fixed during scrolling"
        assert bounds("AgentOffice").x < office.x, "footer actions scroll horizontally"
        screenshot("footer-right")
        for _ in range(8):
            xtest.fake_input(connection, X.ButtonPress, 4)
            xtest.fake_input(connection, X.ButtonRelease, 4)
        connection.sync()
        time.sleep(.5)
        assert bounds("AgentOffice").x == office.x, "scroll back exposes AgentOffice"
        h.pass_check("Settings fixed at far left; AgentOffice before Agents bar; native wheel scrolls other actions")
        click(find("AgentOffice"))
        gui.wait_for(lambda: find("Back to workspace"), "office reopened for adaptive map")
        quiet_pane = h.workspace(socket, quiet_id)["panes"][0]["id"]
        h.send(socket, quiet_pane, "printf '\\033]0;Realtime office\\007'")
        gui.wait_for(lambda: room("Realtime office"), "live workspace name on office sign")
        assert find("Back to workspace"), "renaming must keep AgentOffice open"
        room_names = ["Product studio", "Realtime office"]
        for index in range(6):
            name = f"Studio {index + 3:02}"
            folder = h.root / name
            folder.mkdir()
            h.rpc(socket, "workspace_create", name=name, root=str(folder))
            room_names.append(name)
            gui.wait_for(lambda: room(name), "new room appears without reopening")
        def all_rooms_visible():
            geometry = native_window().get_geometry()
            if geometry.width > 1280 or geometry.height > 900:
                return False
            offices = list(office_nodes())
            for name in room_names:
                node = next((n for n in offices if n.get_role() == Atspi.Role.PUSH_BUTTON
                             and n.get_name().startswith(name + " · ")), None)
                if not node or not node.get_state_set().contains(Atspi.StateType.SHOWING):
                    return False
                rect = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                if rect.x < 0 or rect.y < 0 or rect.x + rect.width > geometry.width or rect.y + rect.height > geometry.height:
                    return False
            return True
        gui.wait_for(all_rooms_visible, "all eight offices fit in one viewport")
        packed_floor("eight-offices")
        # Real disposable agent processes exercise density without touching user sessions.
        for index in range(29):
            name = ("codex", "claude", "gemini")[index % 3]
            tab = h.rpc(socket, "surface_create", workspace=workspace, cwd=str(studio_root))["surface_created"]
            h.send(socket, tab["pane"], f"exec {shlex.quote(str(h.root / name))} 600")
            report((name, tab["pane"], tab["id"]), "working")
        gui.wait_for(lambda: any("32 teammates" in n.get_name() for n in nodes()), "32 agents in shared office")
        gui.wait_for(all_rooms_visible, "dense office leaves every other room visible")
        packed_floor("dense-all-offices")
        click(room("Product studio"))
        time.sleep(.5)
        screenshot("dense-office-detail")
        assert find("All offices")
        click(find("All offices"))
        gui.wait_for(all_rooms_visible, "return from detail restores all offices")
        for width, height in [(900, 900), (1280, 800)]:
            native_window().configure(width=width, height=height)
            connection.sync()
            gui.wait_for(lambda: native_window().get_geometry().width == width, "native resize")
            gui.wait_for(all_rooms_visible, "every office fits after resize")
            packed_floor(f"resized-{width}x{height}")
        # Removing the disposable tab should announce departure, not disappear silently.
        ended_actor = next(n for n in office_nodes() if n.get_role() == Atspi.Role.PUSH_BUTTON
                           and n.get_name().startswith("gemini · ")
                           and "resting" in n.get_name())
        h.rpc(socket, "surface_close", pane=residents[2][1], surface=residents[2][2])
        gui.wait_for(lambda: ended_actor.get_name() == "Session ended", "ended session speech bubble")
        screenshot("session-ended")
        gui.wait_for(lambda: not any(n.get_name() == "Session ended" for n in office_nodes()), "departed character removed", timeout=30)
        h.pass_check("live office names; eight rooms fit; 32 agents; room detail; ended-session bubble and departure")
        click(find("Back to workspace"))
        h.rpc(socket, "workspace_focus", workspace=workspace)
        h.rpc(socket, "surface_focus", pane=residents[0][1], surface=residents[0][2])
        time.sleep(.5)
        screenshot("dense-tabs-first")
        tab_buttons = [n for n in nodes() if n.get_role() == Atspi.Role.PUSH_BUTTON
                       and n.get_name() == "Product studio"]
        assert len(tab_buttons) == 31
        # Traversal is right to left. The first and last tabs must both be reachable.
        first_button, last_button = tab_buttons[-1], tab_buttons[0]
        def tab_visible(button):
            rect = button.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            return 240 <= rect.x < rect.x + rect.width <= 1280
        gui.wait_for(lambda: tab_visible(first_button), "first tab scrolls into view")
        h.rpc(socket, "surface_focus", pane=tab["pane"], surface=tab["id"])
        gui.wait_for(lambda: tab_visible(last_button), "last tab scrolls into view")
        assert native_window().get_geometry().width == 1280
        screenshot("dense-tabs-last")
        h.pass_check("many terminal tabs scroll and reveal selected first/last tabs without growing window")
        h.close_window(process)
    finally:
        if connection is not None:
            connection.close()
        h.cleanup()


if __name__ == "__main__":
    main()
