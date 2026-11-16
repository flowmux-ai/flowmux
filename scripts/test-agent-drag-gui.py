#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Drag an existing Agents row between panes using native input in an isolated GUI."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import signal
import subprocess
import sys
import time

from Xlib import X, XK, display
from Xlib.ext import xtest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "fixture", Path(__file__).with_name("test-ssh-workspace-gui.py"))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--managed", action="store_true", help="Drag Host registry rows and reopen after closing the view")
    parser.add_argument("--tearoff", action="store_true", help="Verify managed native tab tear-off into a registered window")
    parser.add_argument("--host", default="target/debug/flowmux-daemon")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    if args.tearoff and not args.managed:
        parser.error("--tearoff requires --managed")
    for name in ("gui", "cli"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = fixture.Harness(args)
    app = None
    connection = None
    control = None
    host_socket = h.root / "run/flowmux-agent-host.sock"
    try:
        h.env.update(GTK_A11Y="atspi")
        if args.managed:
            h.env.update(FIXTURE_CONTEXT_CLI=args.cli, FIXTURE_CONTEXT_DIR=str(h.root))
        h.env.pop("AT_SPI_BUS_ADDRESS", None)
        config = h.root / "config/flowmux/options.json"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text(json.dumps({"default_shell": "/bin/sh"}))
        source = h.root / "agent.c"
        source.write_text(r'''#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc > 1) { FILE *f = fopen(argv[1], "a"); if (!f) return 1; fprintf(f, "%d\n", getpid()); fclose(f); }
    printf("fixture-pid:%d\n", getpid());
    fflush(stdout);
    char line[512];
    while (fgets(line, sizeof line, stdin)) {
        if (!strcmp(line, "context\n") && getenv("FIXTURE_CONTEXT_CLI")) {
            if (system("\"$FIXTURE_CONTEXT_CLI\" --json identify > \"$FIXTURE_CONTEXT_DIR/identity.tmp\" && mv \"$FIXTURE_CONTEXT_DIR/identity.tmp\" \"$FIXTURE_CONTEXT_DIR/identity.json\"")) return 2;
            if (system("\"$FIXTURE_CONTEXT_CLI\" --json read-screen > \"$FIXTURE_CONTEXT_DIR/screen.tmp\" 2> \"$FIXTURE_CONTEXT_DIR/screen.err\" && mv \"$FIXTURE_CONTEXT_DIR/screen.tmp\" \"$FIXTURE_CONTEXT_DIR/screen.json\"")) return 3;
        }
        printf("ACK:%s", line);
        fflush(stdout);
    }
    return 0;
}
''')
        executable = h.root / "claude"
        subprocess.run(["cc", str(source), "-o", str(executable)], check=True, timeout=30)
        h.start_display()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ.pop("AT_SPI_BUS_ADDRESS", None)
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        if args.managed:
            host_spec = importlib.util.spec_from_file_location("host_fixture", Path(__file__).with_name("test-agent-host-gui.py"))
            host_fixture = importlib.util.module_from_spec(host_spec)
            host_spec.loader.exec_module(host_fixture)
            with (h.root / "host.log").open("w") as log:
                h.spawn([str(Path(args.host).resolve(strict=True)), "--agent-host", "--socket", str(host_socket)], stdout=log, stderr=log)
            fixture.wait_for(host_socket.exists, "isolated Host")
            control = host_fixture.HostClient(host_socket, "relay")
        app, sock = h.window("agent-drag")
        workspace = h.rpc(sock, "workspace_create", root=str(h.root), name="Agent drag fixture")["workspace_created"]["id"]
        h.rpc(sock, "workspace_focus", workspace=workspace)
        pane = h.workspace(sock, workspace)["panes"][0]
        left, surface = pane["id"], pane["tabs"][0]["id"]
        h.rpc(sock, "surface_create", workspace=workspace, cwd=str(h.root))
        h.rpc(sock, "surface_focus", pane=left, surface=surface)
        right = h.rpc(sock, "pane_split", pane=left, direction="vertical")["pane_split_done"]["new_pane"]
        if args.managed:
            launched = subprocess.run([args.cli, "--socket", str(sock), "--json", "agent", "run", "--pane", left,
                "--cwd", str(h.root), "--", str(executable), str(h.root / "starts")], env=h.env,
                text=True, capture_output=True, check=True, timeout=30)
            result = json.loads(launched.stdout)
            agent = result["agent"]
            surface = result["view"]["surface_created"]["id"]
        else:
            h.send(sock, left, "exec " + shlex.quote(str(executable)) + " " + shlex.quote(str(h.root / "starts")))
        pid = int(fixture.wait_for(lambda: re.search(r"fixture-pid:(\d+)", h.screen(sock, left)), "fixture PID").group(1))
        birth = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19]

        def expect_context(pane):
            output = h.root / "identity.json"
            output.unlink(missing_ok=True)
            h.send(sock, pane, "context")
            fixture.wait_for(output.exists, "managed descendant identity")
            identity = json.loads(output.read_text())
            assert identity == dict(pane=pane, surface=surface, workspace=workspace, socket=str(sock)), identity
            output = h.root / "screen.json"
            fixture.wait_for(output.exists, "managed descendant routed screen read")
            screen = json.loads(output.read_text())
            assert f"fixture-pid:{pid}" in json.dumps(screen), screen
            output.unlink()

        if not args.managed:
            h.rpc(sock, "agent_activity_update", pane=left, surface=surface, agent="claude",
                  activity="running", pid=pid, source="flowmux:hook", seq=1, session_id="drag-fixture")
        connection = display.Display(h.env["DISPLAY"])
        window = fixture.wait_for(lambda: next((w for w in connection.screen().root.query_tree().children
            if (owner := w.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)) is not None
            and int(owner.value[0]) == app.pid and w.get_attributes().map_state == X.IsViewable), None), "isolated window")
        window.configure(width=1600, height=1000)
        window.set_input_focus(X.RevertToParent, X.CurrentTime)
        connection.sync()

        def walk(node):
            node.clear_cache()
            yield node
            for i in range(node.get_child_count()):
                child = node.get_child_at_index(i)
                if child:
                    yield from walk(child)

        def nodes():
            desktop = Atspi.get_desktop(0)
            desktop.clear_cache()
            app_node = next((desktop.get_child_at_index(i) for i in range(desktop.get_child_count())
                             if desktop.get_child_at_index(i).get_process_id() == app.pid), None)
            return [node for node in walk(app_node)
                    if node.get_state_set().contains(Atspi.StateType.SHOWING)] if app_node else []

        def handle():
            return next((node for node in nodes() if node.get_role() == Atspi.Role.IMAGE
                and node.get_name() == ("Drag running agent to a pane" if args.managed else "Drag agent tab to a pane")), None)

        def tab(rightmost):
            candidates = [node for node in nodes() if node.get_role() == Atspi.Role.PUSH_BUTTON
                          and node.get_name() == h.root.name]
            return (max if rightmost else min)(candidates,
                key=lambda node: node.get_component_iface().get_extents(Atspi.CoordType.WINDOW).x) if candidates else None

        def point(node, dx=None, dy=None):
            bounds = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            assert bounds.width > 0 and bounds.height > 0
            p = connection.screen().root.translate_coords(window,
                bounds.x + (bounds.width // 2 if dx is None else dx),
                bounds.y + (bounds.height // 2 if dy is None else dy))
            return p.x, p.y

        def drag(destination, cancel=False, source_node=None):
            try:
                node = source_node or fixture.wait_for(handle, "Agents drag handle")
                start = point(node)
                h.log("drag", source=start, destination=destination, role=node.get_role_name(), name=node.get_name())
            except AssertionError:
                (h.root / "accessible-nodes.json").write_text(json.dumps(
                    [{"name": node.get_name(), "role": node.get_role_name()} for node in nodes()], ensure_ascii=False, indent=2))
                raise
            xtest.fake_input(connection, X.MotionNotify, x=start[0], y=start[1])
            # Let GTK dismiss a tooltip before the press can acquire its popup.
            connection.sync()
            time.sleep(.08)
            xtest.fake_input(connection, X.ButtonPress, 1)
            connection.sync()
            for step in range(1, 31):
                xtest.fake_input(connection, X.MotionNotify,
                    x=int(start[0] + (destination[0] - start[0]) * step / 30),
                    y=int(start[1] + (destination[1] - start[1]) * step / 30))
                connection.sync()
                time.sleep(.02)
            time.sleep(.2)
            if cancel:
                key = connection.keysym_to_keycode(XK.string_to_keysym("Escape"))
                xtest.fake_input(connection, X.KeyPress, key)
                connection.sync()
                time.sleep(.15)
                xtest.fake_input(connection, X.KeyRelease, key)
                connection.sync()
                time.sleep(.2)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            connection.sync()

        def located(target):
            panes = h.workspace(sock, workspace)["panes"]
            return len(panes) == 2 and any(p["id"] == target and any(t["id"] == surface for t in p["tabs"]) for p in panes)

        def click(node):
            x, y = point(node)
            xtest.fake_input(connection, X.MotionNotify, x=x, y=y)
            connection.sync()
            time.sleep(.08)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            connection.sync()

        def click_registry():
            click(fixture.wait_for(lambda: next((node for node in nodes() if node.get_role() == Atspi.Role.PUSH_BUTTON
                and "claude · claude" in node.get_name()), None), "registry open button"))

        def find(role, name):
            return next((node for node in nodes() if node.get_role() == role and node.get_name() == name), None)

        def action(name):
            node = fixture.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, name), name)
            assert node.get_action_iface().do_action(0)

        def keypress(name):
            key = connection.keysym_to_keycode(XK.string_to_keysym(name))
            xtest.fake_input(connection, X.KeyPress, key)
            xtest.fake_input(connection, X.KeyRelease, key)
            connection.sync()
            time.sleep(.08)

        def picker():
            return fixture.wait_for(lambda: find(Atspi.Role.COMBO_BOX, "Agent destination"), "Agent destination picker")

        def choose_destination(target=None):
            choices = [(ws["id"], p["id"]) for ws in h.tree(sock)
                       if ws["location"]["type"] != "ssh"
                       for p in ws["panes"] if any(tab["kind"] == "terminal" for tab in p["tabs"])]
            index = next(i for i, (_, pane) in enumerate(choices) if pane == target) if target else len(choices)
            click(picker())
            keypress("Home")
            for _ in range(index):
                keypress("Down")
            keypress("Return")

        def move_menu():
            node = fixture.wait_for(lambda: find(Atspi.Role.TOGGLE_BUTTON, "Agent actions"), "Agent menu")
            assert node.get_action_iface().do_action(0)
            action("Move to…")
            picker()

        def host_agent():
            client = host_fixture.HostClient(host_socket, "relay")
            try:
                return next(a for a in client.call("list")["agents"] if a["instance"] == agent["instance"])
            finally:
                client.close()

        def expect_view(target, visible):
            def matches():
                current = host_agent()
                view = current.get("view")
                return current if view and view["workspace"] == workspace and view["pane"] == target \
                    and view["surface"] == surface and view["visible"] is visible else None
            return fixture.wait_for(matches, f"Host reports current pane {target}, visible={visible}")

        if args.tearoff:
            expect_context(left)
            original_sock, original_workspace, original_surface = sock, workspace, surface
            original_tree = h.tree(sock)
            initial = expect_view(left, True)
            old_key = initial["attachment"]["key"]
            original_window = initial["view"]["window"]
            window.configure(x=100, y=100, width=1200, height=800)
            connection.sync()
            node = fixture.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "claude"), "Agent tab")
            drag((point(node)[0], 20), source_node=node)
            def detached_view():
                view = host_agent().get("view")
                return view if view and view["window"] != original_window else None
            view = fixture.wait_for(detached_view, "registered detached Agent window", timeout=35)
            def detached_native_window():
                for w in connection.screen().root.query_tree().children:
                    owner = w.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                    if owner is not None and int(owner.value[0]) != app.pid and w.get_attributes().map_state == X.IsViewable:
                        return w, int(owner.value[0])
                return None
            torn_window, detached_pid = fixture.wait_for(detached_native_window, "detached native window")
            assert detached_pid not in h.protected_pids
            sock = h.root / f"run/flowmux-{detached_pid}.sock"
            workspace, target, surface = view["workspace"], view["pane"], view["surface"]
            fixture.wait_for(lambda: any(t["id"] == surface for p in h.workspace(sock, workspace)["panes"]
                                        for t in p["tabs"]), "new window registers the tab")
            fixture.wait_for(lambda: not any(t["id"] == original_surface for ws in h.tree(original_sock)
                                            for p in ws["panes"] for t in p["tabs"]), "source closes only its old view")
            for ws in original_tree:
                ws.pop("name")
                for p in ws["panes"]:
                    p["tabs"] = [t for t in p["tabs"] if t["id"] != original_surface]
                    for t in p["tabs"]:
                        t.pop("active")
            actual_tree = h.tree(original_sock)
            for ws in actual_tree:
                ws.pop("name")
                for p in ws["panes"]:
                    for t in p["tabs"]:
                        t.pop("active")
            assert actual_tree == original_tree, (actual_tree, original_tree)
            h.pass_check("native tab tear-off registers a new GUI location and preserves all other source tabs")
            assert host_agent()["attachment"]["key"] != old_key
            assert Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("tear-off transfers the ready relay while preserving the original Agent PID and one launch")
            expect_context(target)
            h.send(sock, target, "detached-input")
            fixture.wait_for(lambda: "ACK:detached-input" in h.screen(sock, target), "detached Agent input")
            h.pass_check("Agent descendants identify the new window and route screen reads and input after tear-off")
            before = h.tree(sock)
            try:
                h.rpc(sock, "agent_view_open", pane=target, instance=agent["instance"], expected_attachment=old_key)
                raise AssertionError("stale move was admitted")
            except RuntimeError as error:
                assert "view changed" in str(error), error
            assert h.tree(sock) == before
            h.pass_check("an outdated move cannot steal the new view or change its layout")
            # Keep the ordinary destination terminal so close-tab has a surviving tab.
            h.rpc(sock, "surface_close", pane=target, surface=surface)
            fixture.wait_for(lambda: host_agent().get("attachment") is None and host_agent().get("view") is None,
                             "detached tab closes to Background")
            assert Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
            h.pass_check("closing the detached tab returns the same execution to Background")
            surface = h.rpc(sock, "agent_view_open", pane=target, instance=agent["instance"])["surface_created"]["id"]
            expect_context(target)
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("Background reopens in the detached window without starting another Agent")
            from Xlib import protocol
            torn_window.send_event(protocol.event.ClientMessage(window=torn_window,
                client_type=connection.intern_atom("WM_PROTOCOLS"),
                data=(32, [connection.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0])))
            connection.sync()
            fixture.wait_for(lambda: not fixture.alive(detached_pid), "detached GUI exits")
            fixture.wait_for(lambda: host_agent().get("view") is None and host_agent().get("attachment") is None,
                             "detached GUI closes to Background")
            assert Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
            h.pass_check("closing the detached window removes its location and leaves the same Agent running")
            sock, workspace = original_sock, original_workspace
            surface = h.rpc(sock, "agent_view_open", pane=left, instance=agent["instance"])["surface_created"]["id"]
            expect_context(left)
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.assert_protected()
            h.pass_check("the original window can reopen the same execution after the detached window exits")
            # Also tear off the final tab: source workspace removal must not lose the new location.
            for p in h.workspace(sock, workspace)["panes"]:
                if p["id"] != left:
                    h.rpc(sock, "pane_close", pane=p["id"])
                else:
                    for tab in p["tabs"]:
                        if tab["id"] != surface:
                            h.rpc(sock, "surface_close", pane=left, surface=tab["id"])
            expect_view(left, True)
            window.set_input_focus(X.RevertToParent, X.CurrentTime)
            connection.sync()
            node = fixture.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "claude"), "final Agent tab")
            drag((point(node)[0], 20), source_node=node)
            view = fixture.wait_for(detached_view, "final tab has a new window", timeout=35)
            fixture.wait_for(lambda: not h.tree(original_sock), "final tab removes its source workspace")
            _, detached_pid = fixture.wait_for(detached_native_window, "second detached window")
            sock = h.root / f"run/flowmux-{detached_pid}.sock"
            workspace, target, surface = view["workspace"], view["pane"], view["surface"]
            expect_context(target)
            assert Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.assert_protected()
            h.pass_check("tearing off the final tab removes the source workspace and retains the same execution and routed context")
            return

        right_tab = fixture.wait_for(lambda: tab(True), "right pane tab")
        fixture.wait_for(handle, "Agents row rendered")
        if args.managed:
            assert not any(node.get_role() == Atspi.Role.LABEL and node.get_name() == "Tab-bound" for node in nodes())
        else:
            fixture.wait_for(lambda: any(node.get_role() == Atspi.Role.LABEL and node.get_name() == "Tab-bound"
                                        for node in nodes()), "legacy ownership badge")
        h.pass_check("Agents visibly distinguishes tab-bound execution from Host-owned execution")
        if args.managed:
            fixture.wait_for(lambda: any(" · Visible" in node.get_name() for node in nodes()), "registry reports the open local view")
            initial_key = expect_view(left, True)["attachment"]["key"]
            h.pass_check("Host verifies the live relay and reports the visible tab location")
            foreign = host_fixture.HostClient(host_socket, "gui")
            try:
                current = host_agent()
                try:
                    foreign.call("report_views", views=[{"key": initial_key, "location": current["view"]}])
                except AssertionError as error:
                    assert "view relay does not belong to this GUI" in str(error), error
                else:
                    raise AssertionError("foreign GUI claimed another GUI's relay")
                assert expect_view(left, True)["attachment"]["key"] == initial_key
            finally:
                foreign.close()
            h.pass_check("a foreign management connection cannot overwrite the owning GUI's location")
            assert app in h.children and app.pid not in h.protected_pids
            os.kill(app.pid, signal.SIGSTOP)
            try:
                fixture.wait_for(lambda: host_agent().get("view") is None, "stale location expires while GUI connection remains open", timeout=10)
                assert host_agent()["attachment"]["key"] == initial_key
            finally:
                os.kill(app.pid, signal.SIGCONT)
            expect_view(left, True)
            h.pass_check("paused GUI reports expire without losing the relay; fresh GTK observations restore Visible")
        # Low in the body would split a regular tab drag; an Agents row opens a tab.
        destination = point(right_tab, dy=600)
        drag(destination)
        fixture.wait_for(lambda: located(right), "Agents row moves live tab to pane body without splitting")
        h.send(sock, right, "first-move")
        fixture.wait_for(lambda: "ACK:first-move" in h.screen(sock, right), "moved PTY input")
        h.pass_check("Agents handle moves the existing tab into a pane body without splitting or restarting")
        if args.managed:
            assert expect_view(right, True)["attachment"]["key"] == initial_key
            stale = dict(key=initial_key, location=dict(host_agent()["view"], pane=left))
            before_tree = h.tree(sock)
            try:
                h.rpc(sock, "in_agent_view", context=stale, request=dict(verb="browser_open", url="about:blank", target_pane=left, direction="vertical"))
                raise AssertionError("stale source admitted a browser mutation")
            except RuntimeError as error:
                assert "no_attached_view" in str(error), error
            assert h.tree(sock) == before_tree
            h.pass_check("GTK rejects a browser effect with a source location from before the drag")
            expect_context(right)
            h.pass_check("After native drag, the actual managed descendant identifies its current pane and surface")
            other = next(t["id"] for p in h.workspace(sock, workspace)["panes"] if p["id"] == right
                         for t in p["tabs"] if t["id"] != surface)
            h.rpc(sock, "surface_focus", pane=right, surface=other)
            expect_view(right, False)
            fixture.wait_for(lambda: any(" · Open" in node.get_name() for node in nodes()), "hidden tab is Open")
            h.rpc(sock, "surface_focus", pane=right, surface=surface)
            expect_view(right, True)
            hidden = h.rpc(sock, "workspace_create", root=str(h.root), name="Hidden view fixture")["workspace_created"]["id"]
            h.rpc(sock, "workspace_focus", workspace=hidden)
            expect_view(right, False)
            h.rpc(sock, "workspace_focus", workspace=workspace)
            expect_view(right, True)
            h.pass_check("drag updates Host location; hidden tabs and workspaces stay Open and return to Visible")
            h.rpc(sock, "pane_focus", pane=left)
            click_registry()
            time.sleep(.3)
            assert located(right), "click moved an existing view back to its old location"
            for name in ("v", "Return"):
                key = connection.keysym_to_keycode(XK.string_to_keysym(name))
                xtest.fake_input(connection, X.KeyPress, key)
                xtest.fake_input(connection, X.KeyRelease, key)
            connection.sync()
            fixture.wait_for(lambda: "ACK:v" in h.screen(sock, right), "registry click focuses the moved view")
            h.pass_check("clicking an open registry row focuses its current pane after a drag")
        drag(point(fixture.wait_for(lambda: tab(False), "left pane tab")))
        fixture.wait_for(lambda: located(left), "second drag moves tab back onto tab strip")
        h.send(sock, left, "second-move")
        fixture.wait_for(lambda: "ACK:second-move" in h.screen(sock, left), "second moved PTY input")
        assert Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
        h.pass_check("repeated drag reads the current pane location and preserves the same PID and input")
        if args.managed:
            expect_context(left)
            def tab_ids(target):
                return next([t["id"] for t in p["tabs"]]
                            for p in h.workspace(sock, workspace)["panes"] if p["id"] == target)

            shells = [t for t in tab_ids(left) if t != surface]
            def visible_tabs():
                row = fixture.wait_for(lambda: tab(False), "visible shell tab").get_component_iface().get_extents(Atspi.CoordType.WINDOW).y
                return sorted([node for node in nodes() if node.get_role() == Atspi.Role.PUSH_BUTTON
                               and node.get_name() in (h.root.name, "claude")
                               and node.get_component_iface().get_extents(Atspi.CoordType.WINDOW).y == row],
                    key=lambda node: node.get_component_iface().get_extents(Atspi.CoordType.WINDOW).x)

            def visible_agent_index():
                return next((i for i, node in enumerate(visible_tabs()) if node.get_name() == "claude"), -1)

            # Target the actual shell tabs' leading/trailing quarters, away from close buttons.
            def shell_edge(last, after):
                candidates = sorted([node for node in nodes() if node.get_role() == Atspi.Role.PUSH_BUTTON
                                     and node.get_name() == h.root.name],
                    key=lambda node: node.get_component_iface().get_extents(Atspi.CoordType.WINDOW).x)
                node = candidates[len(shells) - 1 if last else 0]
                width = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW).width
                return point(node, dx=width * (3 if after else 1) // 4)

            for last, after, expected in [
                (False, False, [surface, *shells]),
                (False, True, [shells[0], surface, shells[1]]),
                (True, True, [*shells, surface]),
                (False, False, [surface, *shells]),
            ]:
                drag(shell_edge(last, after))
                fixture.wait_for(lambda: tab_ids(left) == expected, "same-pane managed tab insertion order")
                fixture.wait_for(lambda: visible_agent_index() == expected.index(surface), "visible GTK tab order matches the store")
            h.send(sock, left, "reordered")
            fixture.wait_for(lambda: "ACK:reordered" in h.screen(sock, left), "reordered PTY input")
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("Agents strip drops reorder the same tab before and after targets in both directions, preserving input and one process")

            right_shells = tab_ids(right)
            for after in [False, True]:
                node = fixture.wait_for(lambda: tab(True), "right shell tab")
                width = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW).width
                drag(point(node, dx=width * (3 if after else 1) // 4))
                expected = [*right_shells, surface] if after else [surface, *right_shells]
                fixture.wait_for(lambda: tab_ids(right) == expected, "cross-pane managed insertion order")
                fixture.wait_for(lambda: visible_agent_index() == len(shells) + expected.index(surface), "visible cross-pane tab order")
                drag(shell_edge(False, False))
                fixture.wait_for(lambda: tab_ids(left) == [surface, *shells], "return to leading tab position")
            assert expect_view(left, True)["attachment"]["key"] == initial_key
            h.pass_check("cross-pane Agents strip drops honor before and after positions without replacing the relay")
            before = tab_ids(left)
            for after in [False, True]:
                node = next(node for node in visible_tabs() if node.get_name() == "claude")
                width = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW).width
                drag(point(node, dx=width * (3 if after else 1) // 4))
                time.sleep(.2)
                assert tab_ids(left) == before and visible_agent_index() == 0
            h.pass_check("dropping an Agent onto either half of its own tab preserves its position without duplication")
            current = host_agent()
            relay = current["attachment"]["relay_pid"]
            assert current["cwd"] == str(h.root) and relay not in h.protected_pids and relay != pid
            os.kill(relay, signal.SIGTERM)
            fixture.wait_for(lambda: host_agent().get("attachment") is None, "isolated relay disconnected")
            drag(shell_edge(True, False))
            expected = [shells[0], surface, shells[1]]
            fixture.wait_for(lambda: tab_ids(left) == expected and visible_agent_index() == 1,
                             "disconnected view reconnects before its current target")
            initial_key = expect_view(left, True)["attachment"]["key"]
            h.send(sock, left, "relay-reordered")
            fixture.wait_for(lambda: "ACK:relay-reordered" in h.screen(sock, left), "reattached reordered input")
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("strip drop reconnects a disconnected relay at the current tab position and preserves the process")
        time.sleep(1)
        window.set_input_focus(X.RevertToParent, X.CurrentTime)
        connection.sync()
        for name in ("z", "Return"):
            key = connection.keysym_to_keycode(XK.string_to_keysym(name))
            xtest.fake_input(connection, X.KeyPress, key)
            connection.sync()
            time.sleep(.1)
            xtest.fake_input(connection, X.KeyRelease, key)
        connection.sync()
        fixture.wait_for(lambda: "ACK:z" in h.screen(sock, left), "native keyboard reaches focused terminal")
        drag(destination, cancel=True)
        time.sleep(.4)
        assert located(left), "cancelled drag changed layout"
        h.send(sock, left, "after-cancel")
        fixture.wait_for(lambda: "ACK:after-cancel" in h.screen(sock, left), "cancel preserves PTY input")
        h.pass_check("Escape cancels an Agents drag and leaves layout and terminal input working")
        if args.managed:
            h.rpc(sock, "surface_close", pane=left, surface=surface)
            fixture.wait_for(lambda: any("Background" in node.get_name() for node in nodes()), "registry row remains in Background")
            assert host_agent().get("attachment") is None and host_agent().get("view") is None
            assert Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
            before = {t["id"] for p in h.workspace(sock, workspace)["panes"] for t in p["tabs"]}
            node = fixture.wait_for(lambda: tab(True), "Background destination tab")
            drag(point(node, dx=5))
            def new_surface():
                return next((t["id"] for p in h.workspace(sock, workspace)["panes"] if p["id"] == right
                    for t in p["tabs"] if t["id"] not in before), None)
            surface = fixture.wait_for(new_surface, "Background drag opens a new view")
            assert tab_ids(right) == [surface, *right_shells]
            fixture.wait_for(lambda: visible_agent_index() == len(shells), "visible Background tab insertion")
            h.send(sock, right, "reopened-background")
            fixture.wait_for(lambda: "ACK:reopened-background" in h.screen(sock, right), "background reattached input")
            assert expect_view(right, True)["attachment"]["key"] != initial_key
            expect_context(right)
            h.pass_check("tab closure keeps the registry row and same PID; Background strip drag inserts before its target without spawning")
            h.rpc(sock, "surface_close", pane=right, surface=surface)
            fixture.wait_for(lambda: any("Background" in node.get_name() for node in nodes()), "second Background state")
            before = {t["id"] for p in h.workspace(sock, workspace)["panes"] for t in p["tabs"]}
            before_tree = h.tree(sock)
            click_registry()
            picker()
            assert h.tree(sock) == before_tree and host_agent().get("attachment") is None
            action("Cancel")
            fixture.wait_for(lambda: find(Atspi.Role.COMBO_BOX, "Agent destination") is None, "picker cancelled")
            assert h.tree(sock) == before_tree and host_agent().get("attachment") is None
            h.pass_check("Background click requires a destination confirmation; cancel changes neither layout nor execution")
            browser_pane = h.rpc(sock, "browser_open", url="about:blank", target_pane=left, direction="vertical")["browser_pane_opened"]["pane"]
            h.rpc(sock, "pane_focus", pane=right)
            h.rpc(sock, "pane_focus", pane=browser_pane)
            click_registry()
            picker()
            action("Open agent")
            surface = fixture.wait_for(new_surface, "registry click opens same execution")
            h.rpc(sock, "pane_close", pane=browser_pane)
            expect_view(right, True)
            h.send(sock, right, "reopened-click")
            fixture.wait_for(lambda: "ACK:reopened-click" in h.screen(sock, right), "clicked view input")
            assert Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("Background click defaults to the recent terminal pane despite browser focus, with fresh input and one process start")
            move_menu()
            before_tree = h.tree(sock)
            choose_destination()
            action("Cancel")
            fixture.wait_for(lambda: find(Atspi.Role.COMBO_BOX, "Agent destination") is None, "new workspace cancelled")
            assert h.tree(sock) == before_tree and located(right)
            h.pass_check("cancelling Move to a new workspace creates no workspace and preserves the open view")
            hidden_pane = h.workspace(sock, hidden)["panes"][0]["id"]
            hidden_shell = h.workspace(sock, hidden)["panes"][0]["tabs"][0]["id"]
            move_menu()
            choose_destination(hidden_pane)
            action("Move agent")
            fixture.wait_for(lambda: any(t["id"] == surface for p in h.workspace(sock, hidden)["panes"]
                                        for t in p["tabs"]), "move to another local workspace")
            h.send(sock, hidden_pane, "menu-move")
            fixture.wait_for(lambda: "ACK:menu-move" in h.screen(sock, hidden_pane), "menu moved input")
            assert any(t["id"] == hidden_shell for p in h.workspace(sock, hidden)["panes"] for t in p["tabs"])
            h.pass_check("Move to another workspace preserves the same Agent tab, process and destination shell")
            move_menu()
            choose_destination(right)
            action("Move agent")
            fixture.wait_for(lambda: located(right), "menu move back")
            # Delete the selected destination while the modal is open. Confirmation must not fall back.
            doomed_pane = h.rpc(sock, "pane_split", pane=right, direction="vertical")["pane_split_done"]["new_pane"]
            move_menu()
            choose_destination(doomed_pane)
            h.rpc(sock, "pane_close", pane=doomed_pane)
            before_tree = h.tree(sock)
            action("Move agent")
            fixture.wait_for(lambda: any("destination changed or closed" in node.get_name() for node in nodes()), "stale destination rejected")
            assert h.tree(sock) == before_tree and located(right)
            action("OK")
            h.pass_check("a destination closed during selection is rejected without moving the view or choosing another pane")
            move_menu()
            choose_destination()
            before_workspaces = {ws["id"] for ws in h.tree(sock)}
            action("Move agent")
            created = fixture.wait_for(lambda: next((ws for ws in h.tree(sock) if ws["id"] not in before_workspaces), None), "confirmed new workspace")
            created_pane = created["panes"][0]["id"]
            fixture.wait_for(lambda: any(t["id"] == surface for p in h.workspace(sock, created["id"])["panes"] for t in p["tabs"]), "same view in new workspace")
            h.send(sock, created_pane, "new-workspace")
            fixture.wait_for(lambda: "ACK:new-workspace" in h.screen(sock, created_pane), "new workspace input")
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("confirmed New workspace moves the same execution and accepts fresh input without provider resume")
            move_menu()
            choose_destination(right)
            action("Move agent")
            fixture.wait_for(lambda: located(right), "return from new workspace")
            click(fixture.wait_for(lambda: next((node for node in nodes() if node.get_name() == "Agents bar"
                and node.get_role() == Atspi.Role.TOGGLE_BUTTON), None), "Agents bar toggle"))
            drag(point(fixture.wait_for(lambda: tab(False), "left pane tab")))
            fixture.wait_for(lambda: located(left), "managed bar moves existing view")
            h.send(sock, left, "bar-move")
            fixture.wait_for(lambda: "ACK:bar-move" in h.screen(sock, left), "bar moved PTY input")
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("Agents bar also exposes the registry and moves the same running process")
            expect_view(left, True)
            h.close_window(app)
            fixture.wait_for(lambda: host_agent().get("view") is None and host_agent().get("attachment") is None,
                             "GUI closure removes location and relay without ending execution")
            assert Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
            h.pass_check("closing the isolated GUI immediately clears its location and retains the same running Agent")
    finally:
        if args.tearoff:
            # Tear-off children inherit only this fixture's private runtime directory.
            for proc in Path("/proc").iterdir():
                if not proc.name.isdigit() or (app and int(proc.name) == app.pid):
                    continue
                try:
                    owned = (f"XDG_RUNTIME_DIR={h.root / 'run'}".encode() in (proc / "environ").read_bytes().split(b"\0")
                             and (proc / "exe").resolve() == Path(args.gui))
                    if owned:
                        assert int(proc.name) not in h.protected_pids
                        os.kill(int(proc.name), signal.SIGTERM)
                except (FileNotFoundError, ProcessLookupError, PermissionError):
                    pass
        if connection:
            connection.close()
        try:
            if control:
                control.close()
            if control and host_socket.exists():
                control = host_fixture.HostClient(host_socket, "relay")
                try:
                    for agent in control.call("list")["agents"]:
                        assert agent["cwd"] == str(h.root), agent
                        control.call("terminate", instance=agent["instance"], epoch=agent["epoch"], mode="terminate")
                    fixture.wait_for(lambda: not host_socket.exists(), "fixture Host reaps its executions", timeout=10)
                finally:
                    control.close()
        finally:
            h.cleanup()


if __name__ == "__main__":
    main()
