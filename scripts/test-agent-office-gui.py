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
import uuid

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

        def shown(text):
            node = find(text)
            return node is not None and node.get_state_set().contains(Atspi.StateType.SHOWING)

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

        def pointer_click(x, y):
            point = connection.screen().root.translate_coords(native_window(), int(x), int(y))
            xtest.fake_input(connection, X.MotionNotify, x=point.x, y=point.y)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            connection.sync()

        def assert_stationary(actor, description):
            # Allow GTK to allocate the new view, then detect unwanted walking.
            time.sleep(.2)
            before = actor.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            time.sleep(.6)
            after = actor.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            h.log("stationary_actor", check=description, name=actor.get_name(),
                  before=(before.x, before.y), after=(after.x, after.y))
            if (before.x, before.y) != (after.x, after.y):
                screenshot("unexpected-character-motion")
            assert (before.x, before.y) == (after.x, after.y), description

        def screenshot(name):
            time.sleep(.3)
            root = connection.screen().root
            geometry = root.get_geometry()
            raw = root.get_image(0, 0, geometry.width, geometry.height, X.ZPixmap, 0xffffffff)
            Image.frombytes("RGB", (geometry.width, geometry.height), raw.data,
                            "raw", "BGRX").save(h.root / f"{name}.png")

        def packed_floor(name):
            # Offices keep fixed 16:9 floor plans at one shared scale, so the map
            # letterboxes rather than stretching rooms; it must still use the window
            # well, and office signs (one per room) must not overlap.
            screenshot(name)
            geometry = native_window().get_geometry()
            picture = Image.open(h.root / f"{name}.png")
            floor = picture.crop((4, 92, geometry.width - 4, geometry.height - 44))
            pixels = floor.width * floor.height
            empty = sum(count for count, color in floor.getcolors(pixels) if color == (17, 25, 35))
            assert empty / pixels < .45, f"Offices leave {empty / pixels:.1%} unused space"
            # AT-SPI can list one widget twice; compare each distinct sign once.
            signs = {}
            for n in office_nodes():
                if (n.get_role() == Atspi.Role.PUSH_BUTTON and " · " in n.get_name()
                        and n.get_name().split(" · ")[-1].isdigit()
                        and n.get_state_set().contains(Atspi.StateType.SHOWING)):
                    r = n.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                    signs[n.get_name()] = r
            h.log("office_signs", name=name, signs={k: (r.x, r.y, r.width, r.height) for k, r in signs.items()})
            signs = list(signs.values())
            for index, a in enumerate(signs):
                for b in signs[index + 1:]:
                    assert a.x + a.width <= b.x or b.x + b.width <= a.x or a.y + a.height <= b.y \
                        or b.y + b.height <= a.y, "office signs must not overlap"

        studio_root = h.root / "Product studio"
        quiet_root = h.root / "Quiet corner"
        studio_root.mkdir()
        quiet_root.mkdir()
        workspace = h.rpc(socket, "workspace_create", name="Product studio", root=str(studio_root))["workspace_created"]["id"]
        click(gui.wait_for(lambda: find("AgentOffice"), "footer button"))
        gui.wait_for(lambda: find("Back to workspace"), "empty office open")
        assert not room("Product studio")
        assert find("0 offices · 0 teammates · 0 working · 0 need you · 0 resting")
        screenshot("empty-office")
        click(find("Back to workspace"))
        residents = []
        for index, name in enumerate(("codex", "claude")):
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
        assert room("Product studio") and not room("Quiet corner")
        # Exercise desk reflow and zoning: a tall room can fit a desk row above the lounge.
        for _ in range((-uuid.UUID(workspace).int) % 48):
            click(find("Next office design"))
            time.sleep(.3)
        actors = [n for n in office_nodes() if n.get_role() == Atspi.Role.PUSH_BUTTON
                  and any(n.get_name().startswith(name + " · ") for name in ("codex", "claude", "gemini"))]
        assert len(actors) == 2
        # Extreme ratios that turn desk rows into columns are covered by unit tests; here
        # both seats must stay distinct and visible as the window reshapes the room.
        for width, height in [(800, 950), (1280, 700)]:
            native_window().configure(width=width, height=height)
            connection.sync()
            gui.wait_for(lambda: native_window().get_geometry().width == width, "ratio resize")
            def desks_apart():
                a, b = (actor.get_component_iface().get_extents(Atspi.CoordType.WINDOW) for actor in actors)
                apart = a.x + a.width <= b.x or b.x + b.width <= a.x or a.y + a.height <= b.y or b.y + b.height <= a.y
                return apart and min(a.width, b.width) > 0
            try:
                gui.wait_for(desks_apart, "desks reflow without overlapping at each ratio")
            finally:
                screenshot(f"desk-ratio-{width}x{height}")
                h.log("desk_ratio", width=width, height=height,
                      actors=[(actor.get_name(), actor.get_component_iface().get_extents(Atspi.CoordType.WINDOW).x)
                              for actor in actors])
        h.pass_check("empty workspaces hidden; desks and zoning reflow with available ratio")
        click(room("Product studio"))
        assert not shown("All offices"), "one office must hide All offices even in detail"
        assert_stationary(actors[0], "selecting the sole office must not restart walking")
        click(room("Product studio"))
        assert not shown("All offices")
        design_file = h.root / "state/flowmux/office-designs" / f"{workspace}.json"
        for design in range(1, 49):
            click(find("Next office design"))
            gui.wait_for(lambda: design_file.exists() and json.loads(design_file.read_text()) == design % 48,
                         "design change applied before checking character position")
            assert_stationary(actors[0], "changing design must keep a working character at its desk")
        report(residents[0], "working", "Refresh same activity")
        assert_stationary(actors[0], "refreshing the model must preserve a working character's position")
        h.pass_check("single office hides All offices; design changes and model refresh keep characters seated")
        # Three-seat sofas used to have overlapping speech-sized character buttons.
        for resident in residents:
            report(resident, "idle")
        time.sleep(5)
        seated = [actor.get_component_iface().get_extents(Atspi.CoordType.WINDOW) for actor in actors]
        a, b = sorted(seated, key=lambda r: r.x)
        h.log("seated_hitboxes", actors=[(r.x, r.y, r.width, r.height) for r in seated])
        screenshot("adjacent-character-hitboxes")
        assert abs(a.y - b.y) <= 2, "both characters must be seated on the same sofa"
        assert a.x + a.width <= b.x + 1, "adjacent character hit areas must not overlap"
        assert all(r.height > r.width * 2 for r in seated), "selection fits the body, not speech"
        # This space above a character belonged to its old speech-sized button.
        pointer_click(a.x + a.width / 2, a.y - a.height / 4)
        time.sleep(.3)
        assert find("Back to workspace"), "empty space above a character must not select it"
        # Nameplates are separate noninteractive decorations.
        pointer_click(a.x + a.width / 2, a.y + a.height + 4)
        time.sleep(.3)
        assert find("Back to workspace"), "nameplate must not select the character"
        for name, pane, surface in residents:
            target = next(n for n in office_nodes() if n.get_role() == Atspi.Role.PUSH_BUTTON
                          and n.get_name().startswith(name + " · "))
            rect = target.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            pointer_click(rect.x + rect.width / 2, rect.y + rect.height * .65)
            gui.wait_for(lambda: find("Back to workspace") is None, "native character click opens terminal")
            tabs = h.workspace(socket, workspace)["panes"][0]["tabs"]
            assert next(tab for tab in tabs if tab["id"] == surface)["active"], name
            click(gui.wait_for(lambda: find("AgentOffice"), "reopen after character click"))
            gui.wait_for(lambda: find("Back to workspace"), "office reopened")
            time.sleep(.3)
        for resident in residents:
            report(resident, "working")
        h.pass_check("adjacent body hitboxes do not overlap; native clicks select each tab; labels do not select")
        tab = h.rpc(socket, "surface_create", workspace=workspace, cwd=str(studio_root))["surface_created"]
        executable = h.root / "gemini"
        shutil.copyfile(shutil.which("sleep"), executable)
        executable.chmod(0o755)
        h.send(socket, tab["pane"], f"exec {shlex.quote(str(executable))} 600")
        residents.append(("gemini", tab["pane"], tab["id"]))
        report(residents[2], "working")
        screenshot("working")
        report(residents[1], "blocked", "승인 대기 / Review changes")
        gui.wait_for(lambda: next((n for n in nodes() if "Needs you · waiting for input" in n.get_name()), None), "blocked scene")
        gemini = next(n for n in nodes() if n.get_role() == Atspi.Role.PUSH_BUTTON and n.get_name().startswith("gemini · "))
        before = gemini.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        report(residents[2], "idle")
        gui.wait_for(lambda: next((n for n in nodes() if "Done · taking a break" in n.get_name()), None), "completed scene")
        # A small native resize during the walk must not snap to the lounge.
        walking_before = gemini.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        geometry = native_window().get_geometry()
        started = time.monotonic()
        native_window().configure(width=geometry.width - 4, height=geometry.height)
        connection.sync()
        time.sleep(.1)
        walking_after = gemini.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        elapsed = time.monotonic() - started
        distance = ((walking_after.x - walking_before.x) ** 2 + (walking_after.y - walking_before.y) ** 2) ** .5
        assert distance < 60 + elapsed * 400, f"resize teleported actor by {distance}px"
        native_window().configure(width=geometry.width, height=geometry.height)
        connection.sync()
        time.sleep(5)
        after = gemini.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        assert (before.x, before.y) != (after.x, after.y), "completed character walks to lounge"
        screenshot("mixed-status")
        report(residents[0], "working", "Read README.md")
        time.sleep(2)
        screenshot("reading-at-desk")
        report(residents[0], "working", "Edit scene.rs")
        time.sleep(8)
        screenshot("coffee-break")
        h.pass_check("live workspaces, three agent providers, working / blocked / completed scenes")

        target = gui.wait_for(lambda: next((node for node in nodes()
            if node.get_role() == Atspi.Role.PUSH_BUTTON
            and node.get_name().startswith("claude · ")), None), "Claude character")
        bubble = next(n for n in office_nodes() if n.get_role() == Atspi.Role.LABEL
                      and n.get_name().startswith("Need your input")
                      and n.get_state_set().contains(Atspi.StateType.SHOWING))
        rect = bubble.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        pointer_click(rect.x + rect.width / 2, rect.y + rect.height / 2)
        time.sleep(.3)
        assert find("Back to workspace"), "speech bubble must not select a character"
        rect = target.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
        pointer_click(rect.x + rect.width / 2, rect.y + rect.height * .65)
        gui.wait_for(lambda: find("Back to workspace") is None, "character returns to terminal")
        current = h.rpc(socket, "workspace_current")["workspace_current"]["id"]
        assert current == workspace
        tabs = h.workspace(socket, workspace)["panes"][0]["tabs"]
        assert next(tab for tab in tabs if tab["id"] == residents[1][2])["active"]
        click(gui.wait_for(lambda: find("AgentOffice"), "footer button after return"))
        gui.wait_for(lambda: find("Back to workspace"), "office reopened")
        design_file = h.root / "state/flowmux/office-designs" / f"{workspace}.json"
        # The first selection was West wing; recreation must preserve it.
        if design_file.exists():
            assert json.loads(design_file.read_text()) == 0
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
        assert not room("Quiet corner")
        quiet_surface = h.workspace(socket, quiet_id)["panes"][0]["tabs"][0]["id"]
        h.send(socket, quiet_pane, f"exec {shlex.quote(str(h.root / 'codex'))} 600")
        report(("codex", quiet_pane, quiet_surface), "working")
        gui.wait_for(lambda: room("Quiet corner"), "agent makes hidden workspace appear")
        # A separate shell can rename the workspace while the agent keeps running.
        quiet_shell = h.rpc(socket, "surface_create", workspace=quiet_id, cwd=str(quiet_root))["surface_created"]
        h.send(socket, quiet_shell["pane"], "printf '\\033]0;Realtime office\\007'")
        gui.wait_for(lambda: room("Realtime office"), "live workspace name on office sign")
        assert find("Back to workspace"), "renaming must keep AgentOffice open"
        room_names = ["Product studio", "Realtime office"]
        for index in range(6):
            name = f"Studio {index + 3:02}"
            folder = h.root / name
            folder.mkdir()
            added = h.rpc(socket, "workspace_create", name=name, root=str(folder))["workspace_created"]["id"]
            assert not room(name)
            added_pane = h.workspace(socket, added)["panes"][0]
            added_surface = added_pane["tabs"][0]["id"]
            h.send(socket, added_pane["id"], f"exec {shlex.quote(str(h.root / 'codex'))} 600")
            report(("codex", added_pane["id"], added_surface), "working")
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
        click(room("Product studio"))
        worker = next(n for n in office_nodes() if n.get_role() == Atspi.Role.PUSH_BUTTON
                      and n.get_name().startswith("codex · ")
                      and "Working · at the desk" in n.get_name()
                      and n.get_state_set().contains(Atspi.StateType.SHOWING))
        gui.wait_for(lambda: shown("All offices"), "office detail selected")
        assert_stationary(worker, "office selection must keep the worker at its desk")
        click(find("All offices"))
        gui.wait_for(lambda: not shown("All offices"), "overview selected")
        assert_stationary(worker, "returning to all offices must not restart walking")
        h.pass_check("office selection and All offices preserve settled character positions")
        # Real disposable agent processes exercise density without touching user sessions.
        dense_residents = []
        for index in range(29):
            name = ("codex", "claude", "gemini")[index % 3]
            tab = h.rpc(socket, "surface_create", workspace=workspace, cwd=str(studio_root))["surface_created"]
            h.send(socket, tab["pane"], f"exec {shlex.quote(str(h.root / name))} 600")
            dense_residents.append((name, tab["pane"], tab["id"]))
            report(dense_residents[-1], "working")
        gui.wait_for(lambda: any("39 teammates" in n.get_name() for n in nodes()), "32 agents in shared office")
        gui.wait_for(all_rooms_visible, "dense office leaves every other room visible")
        packed_floor("dense-all-offices")
        click(room("Product studio"))
        time.sleep(.5)
        screenshot("dense-office-detail")
        for index, resident in enumerate(dense_residents[:8]):
            report(resident, "blocked", f"Review request {index}")
        time.sleep(3)
        labels = [n for n in office_nodes() if n.get_role() == Atspi.Role.LABEL
                  and n.get_name().startswith("Need your input")
                  and n.get_state_set().contains(Atspi.StateType.SHOWING)]
        # AT-SPI can report a label twice, or a stale extent while it is re-measured;
        # compare each on-screen bubble once.
        unique = {}
        for n in labels:
            r = n.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            if r.width >= 40 and r.height >= 20:
                unique[(r.x, r.y, r.width, r.height)] = r
        bubbles = list(unique.values())
        assert len(bubbles) >= 2, "dense office must expose multiple pending approvals"
        screenshot("dense-approvals")
        h.log("approval_bounds", bubbles=[(r.x, r.y, r.width, r.height) for r in bubbles])
        for index, a in enumerate(bubbles):
            assert a.width >= 110 and a.height >= 30, "speech stays readable when office shrinks"
            for b in bubbles[index + 1:]:
                assert a.x + a.width <= b.x or b.x + b.width <= a.x or a.y + a.height <= b.y or b.y + b.height <= a.y, "approval bubbles must not overlap"
        for resident in dense_residents[:8]:
            report(resident, "working")
        h.pass_check("multiple approval bubbles remain readable without overlapping")
        design_file = h.root / "state/flowmux/office-designs" / f"{workspace}.json"
        for design in range(1, 49):
            click(next(n for n in office_nodes() if n.get_name() == "Next office design"
                       and n.get_state_set().contains(Atspi.StateType.SHOWING)))
            gui.wait_for(lambda: design_file.exists() and json.loads(design_file.read_text()) == design % 48,
                         "workspace design persisted")
            screenshot(f"layout-{design % 48:02}")
        click(find("Back to workspace"))
        click(gui.wait_for(lambda: find("AgentOffice"), "reopen saved office"))
        gui.wait_for(lambda: room("Product studio"), "saved office restored")
        assert json.loads(design_file.read_text()) == 0
        h.pass_check("48 office designs render with 32 agents; workspace design survives reopening")
        assert not shown("All offices"), "overview must hide the redundant All offices button"
        click(room("Product studio"))
        gui.wait_for(lambda: shown("All offices"), "detail exposes All offices")
        click(find("All offices"))
        gui.wait_for(lambda: not shown("All offices"), "return to overview hides All offices")
        gui.wait_for(all_rooms_visible, "return from detail restores all offices")
        for width, height in [(900, 900), (1280, 800)]:
            native_window().configure(width=width, height=height)
            connection.sync()
            gui.wait_for(lambda: native_window().get_geometry().width == width, "native resize")
            gui.wait_for(all_rooms_visible, "every office fits after resize")
            packed_floor(f"resized-{width}x{height}")
        # Losing a selected office's last agent returns to the remaining offices.
        click(room("Realtime office"))
        def others_hidden():
            other = room("Product studio")
            return other is None or not other.get_state_set().contains(Atspi.StateType.SHOWING)
        gui.wait_for(others_hidden, "single-agent office is enlarged before removal")
        h.rpc(socket, "surface_close", pane=quiet_pane, surface=quiet_surface)
        gui.wait_for(lambda: room("Realtime office") is None, "last agent hides selected office")
        assert h.workspace(socket, quiet_id), "empty workspace itself must remain"
        room_names.remove("Realtime office")
        gui.wait_for(all_rooms_visible, "remaining offices reappear after selected office empties")
        assert not shown("All offices"), "removing selected office returns to overview without All offices"
        h.pass_check("All offices only visible in detail; overview and selected office removal hide it")
        packed_floor("empty-room-removed")
        h.send(socket, quiet_shell["pane"], f"exec {shlex.quote(str(h.root / 'codex'))} 600")
        report(("codex", quiet_shell["pane"], quiet_shell["id"]), "working")
        room_names.append("Realtime office")
        gui.wait_for(all_rooms_visible, "office reappears when an agent rejoins")
        h.pass_check("last agent hides selected office and repacks map; joining agent restores office")
        # Removing one agent from an occupied office still announces departure.
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
            parent = button.get_parent()
            while parent and parent.get_role() != Atspi.Role.SCROLL_PANE:
                parent = parent.get_parent()
            assert parent, "terminal tab must belong to a scroll viewport"
            viewport = parent.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            return viewport.x <= rect.x < rect.x + rect.width <= viewport.x + viewport.width
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
