#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Delay actual relay snapshot writes; verify readiness, cancellation and recovery."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import time
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


gui = module("gui", "test-ssh-workspace-gui.py")
host_fixture = module("host_fixture", "test-agent-host-gui.py")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--host", default="target/debug/flowmux-daemon")
    parser.add_argument("--replacement", action="store_true",
                        help="verify explicit relay handoff between two live VTE windows")
    parser.add_argument("--gui-replacement", action="store_true",
                        help="verify product GUI admission, destination selection and cross-window drag")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for name in ("gui", "cli", "host"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = gui.Harness(args)
    control = None
    try:
        source = h.root / "delay.c"
        source.write_text(r'''
#define _GNU_SOURCE
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
ssize_t write(int fd, const void *data, size_t count) {
    static ssize_t (*real_write)(int, const void *, size_t);
    static int relay;
    if (!real_write) real_write = dlsym(RTLD_NEXT, "write");
    if (!relay) {
        char args[8192];
        int cmd = open("/proc/self/cmdline", O_RDONLY);
        ssize_t size = cmd < 0 ? 0 : read(cmd, args, sizeof(args));
        if (cmd >= 0) close(cmd);
        relay = -1;
        for (ssize_t i = 0; i + 8 <= size; ++i) {
            if (!memcmp(args + i, "pty-tee\0", 8)) break;
            if (i + 12 <= size && !memcmp(args + i, "agent-relay\0", 12)) {
                relay = 1;
                break;
            }
        }
    }
    struct stat output, target;
    if (relay == 1 && fd != 2 && !fstat(1, &output) && !fstat(fd, &target)
            && output.st_dev == target.st_dev && output.st_ino == target.st_ino
            && getenv("FIXTURE_HOLD")) {
        if (!access(getenv("FIXTURE_HOLD"), F_OK)) {
            int marker = open(getenv("FIXTURE_WRITING"), O_WRONLY | O_CREAT, 0600);
            if (marker >= 0) { dprintf(marker, "%d\n", getpid()); close(marker); }
            while (!access(getenv("FIXTURE_HOLD"), F_OK)) usleep(10000);
        }
        if (!access(getenv("FIXTURE_FAIL"), F_OK)) {
            errno = EPIPE;
            return -1;
        }
    }
    return real_write(fd, data, count);
}
''')
        preload = h.root / "delay.so"
        subprocess.run(["cc", "-shared", "-fPIC", str(source), "-ldl", "-o", str(preload)],
                       check=True, timeout=30)
        hold, writing, fail = (h.root / name for name in ("hold", "writing", "fail"))
        h.env.update(GTK_A11Y="atspi", LD_PRELOAD=str(preload), FIXTURE_HOLD=str(hold),
                     FIXTURE_WRITING=str(writing), FIXTURE_FAIL=str(fail))
        options = h.root / "config/flowmux/options.json"
        options.parent.mkdir(parents=True, exist_ok=True)
        options.write_text(json.dumps({"default_shell": "/bin/sh"}))
        h.start_display()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ.pop("AT_SPI_BUS_ADDRESS", None)
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        host_socket = h.root / "run/flowmux-agent-host.sock"
        with (h.root / "host.log").open("w") as log:
            h.spawn([args.host, "--agent-host", "--socket", str(host_socket)], stdout=log, stderr=log)
        gui.wait_for(host_socket.exists, "Host socket")
        control = host_fixture.HostClient(host_socket, "relay")
        agent = control.call("launch", launch={
            "request_id": host_fixture.launch_request_id(), "provider": "fixture", "label": "Ready fixture",
            "argv": ["/bin/sh", "-c", "stty -echo; printf 'READY-SNAPSHOT\\n'; while IFS= read -r line; do printf 'ACK:%s\\n' \"$line\"; done"],
            "cwd": str(h.root), "env": list(h.env.items()), "size": {"rows": 24, "cols": 80},
        })["agent"]
        pid, birth = agent["pid"], agent["process_start"]
        app, sock = h.window("agent-ready")
        workspace = h.rpc(sock, "workspace_create", root=str(h.root), name="Ready fixture")["workspace_created"]["id"]
        h.rpc(sock, "workspace_focus", workspace=workspace)
        pane = h.workspace(sock, workspace)["panes"][0]["id"]

        def current():
            return next(item for item in control.call("list")["agents"] if item["instance"] == agent["instance"])

        def tabs():
            return h.workspace(sock, workspace)["panes"][0]["tabs"]

        def label(text):
            def walk(node):
                if (node.get_role() == Atspi.Role.LABEL and node.get_name() == text
                        and node.get_state_set().contains(Atspi.StateType.SHOWING)):
                    return node
                for index in range(node.get_child_count()):
                    child = node.get_child_at_index(index)
                    if child and (found := walk(child)):
                        return found
            return walk(Atspi.get_desktop(0))

        def preserves_execution():
            value = current()
            assert value["state"] == "running" and (value["pid"], value["process_start"]) == (pid, birth)
            assert int(Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19]) == birth

        if args.gui_replacement:
            opened = h.rpc(sock, "agent_view_open", pane=pane, instance=agent["instance"])
            original_surface = opened["surface_created"]["id"]
            gui.wait_for(lambda: current()["view"], "source GUI location")
            original = current()["attachment"]["key"]
            # Only destination relays are delayed; the original VTE must remain interactive.
            hold, writing, fail = (h.root / ("gui-" + name) for name in ("hold", "writing", "fail"))
            h.env.update(FIXTURE_HOLD=str(hold), FIXTURE_WRITING=str(writing), FIXTURE_FAIL=str(fail))
            other, other_sock = h.window("gui-move-destination")
            other_workspace = h.rpc(other_sock, "workspace_create", root=str(h.root),
                                    name="GUI move destination")["workspace_created"]["id"]
            h.rpc(other_sock, "workspace_focus", workspace=other_workspace)
            destination = h.workspace(other_sock, other_workspace)["panes"][0]["id"]

            def source_input():
                token = "source-" + uuid.uuid4().hex
                h.send(sock, pane, token)
                gui.wait_for(lambda: "ACK:" + token in h.screen(sock, pane), "fresh original VTE input")
                value = current()
                assert value["attachment"]["key"] == original and value["view"]["pane"] == pane
                assert any(tab["id"] == original_surface for tab in tabs())
                preserves_execution()

            with ThreadPoolExecutor(max_workers=1) as pool:
                for outcome in ("close", "write-failure", "timeout", "ready"):
                    writing.unlink(missing_ok=True)
                    hold.touch()
                    future = pool.submit(h.rpc, other_sock, "agent_view_open", pane=destination,
                                         instance=agent["instance"])
                    gui.wait_for(lambda: writing.exists() and writing.read_text().strip().isdigit(),
                                 "product destination relay blocked on snapshot write")
                    candidate_pid = int(writing.read_text())
                    (h.root / ("gui-" + outcome + "-pid")).write_text(str(candidate_pid))
                    argv = Path(f"/proc/{candidate_pid}/cmdline").read_bytes().split(b"\0")
                    assert b"--ready-token" in argv and b"--replace-lease" in argv, argv
                    candidate = next(tab["id"] for item in h.workspace(other_sock, other_workspace)["panes"]
                                     for tab in item["tabs"] if tab["title"] == agent["label"])
                    assert not future.done(), "move succeeded before destination snapshot was written"
                    source_input()
                    if outcome == "close":
                        h.rpc(other_sock, "surface_close", pane=destination, surface=candidate)
                    elif outcome == "write-failure":
                        fail.touch()
                    elif outcome == "timeout":
                        try:
                            future.result(timeout=15)
                            raise AssertionError("unacknowledged product move returned success")
                        except RuntimeError as error:
                            assert "timed out" in str(error), error
                        source_input()
                    hold.unlink()
                    if outcome != "ready":
                        try:
                            future.result(timeout=8)
                            raise AssertionError("failed product destination returned success")
                        except RuntimeError as error:
                            assert any(text in str(error) for text in ("before connection", "disconnected", "timed out")), error
                        if outcome != "close":
                            h.rpc(other_sock, "surface_close", pane=destination, surface=candidate)
                        fail.unlink(missing_ok=True)
                        gui.wait_for(lambda: not Path(f"/proc/{candidate_pid}").exists(), "failed product relay reaped")
                        source_input()
                        h.pass_check(f"Product GUI move {outcome} rejects the destination and preserves original VTE input, location and exact PID")
                        continue
                    assert future.result(timeout=12)["surface_created"]["id"] == candidate
                    gui.wait_for(lambda: current()["view"] and current()["view"]["pane"] == destination,
                                 "admitted product GUI location")
                    assert current()["attachment"]["key"]["generation"] > original["generation"]
                    gui.wait_for(lambda: all(tab["id"] != original_surface for tab in tabs()), "old tab removed after GUI admission")
                    h.send(other_sock, destination, "gui-admitted")
                    gui.wait_for(lambda: "ACK:gui-admitted" in h.screen(other_sock, destination), "admitted GUI input")
                    preserves_execution()
                    h.pass_check("Product GUI commits a prepared move only after destination admission and removes the old tab")

            # Exercise the actual selector and native cross-window Agents drag, not just IPC.
            from Xlib.ext import xtest
            from Xlib import XK
            connection = gui.display.Display(h.env["DISPLAY"])
            try:
                def window(process):
                    atom = connection.intern_atom("_NET_WM_PID")
                    return next((item for item in connection.screen().root.query_tree().children
                                 if (owner := item.get_full_property(atom, gui.X.AnyPropertyType)) is not None
                                 and int(owner.value[0]) == process.pid and item.get_attributes().map_state == gui.X.IsViewable), None)
                left = gui.wait_for(lambda: window(app), "source mapped window")
                right = gui.wait_for(lambda: window(other), "destination mapped window")
                left.configure(x=0, y=0, width=800, height=950)
                right.configure(x=800, y=0, width=800, height=950)
                connection.sync()

                def nodes(process):
                    def walk(node):
                        node.clear_cache()
                        yield node
                        for index in range(node.get_child_count()):
                            child = node.get_child_at_index(index)
                            if child:
                                yield from walk(child)
                    desktop = Atspi.get_desktop(0)
                    desktop.clear_cache()
                    node = next((desktop.get_child_at_index(i) for i in range(desktop.get_child_count())
                                 if desktop.get_child_at_index(i).get_process_id() == process.pid), None)
                    return [item for item in walk(node) if item.get_state_set().contains(Atspi.StateType.SHOWING)] if node else []

                def find(process, role, name):
                    return next((node for node in nodes(process) if node.get_role() == role and node.get_name() == name), None)

                def action(process, role, name):
                    node = gui.wait_for(lambda: find(process, role, name), name)
                    assert node.get_action_iface().do_action(0)

                def press(name):
                    key = connection.keysym_to_keycode(XK.string_to_keysym(name))
                    xtest.fake_input(connection, gui.X.KeyPress, key)
                    xtest.fake_input(connection, gui.X.KeyRelease, key)
                    connection.sync()
                    time.sleep(.08)

                left.set_input_focus(gui.X.RevertToParent, gui.X.CurrentTime)
                action(app, Atspi.Role.TOGGLE_BUTTON, "Agent actions")
                action(app, Atspi.Role.PUSH_BUTTON, "Move to…")
                picker = gui.wait_for(lambda: find(app, Atspi.Role.COMBO_BOX, "Agent destination"), "GUI move picker")
                choices = [item["id"] for ws in h.tree(sock) if ws["location"]["type"] != "ssh"
                           for item in ws["panes"] if any(tab["kind"] == "terminal" for tab in item["tabs"])]
                bounds = picker.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                point = connection.screen().root.translate_coords(left, bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
                xtest.fake_input(connection, gui.X.MotionNotify, x=point.x, y=point.y)
                xtest.fake_input(connection, gui.X.ButtonPress, 1)
                xtest.fake_input(connection, gui.X.ButtonRelease, 1)
                connection.sync()
                press("Home")
                for _ in range(choices.index(pane)):
                    press("Down")
                press("Return")
                action(app, Atspi.Role.PUSH_BUTTON, "Move agent")
                gui.wait_for(lambda: current()["view"] and current()["view"]["pane"] == pane, "selector moves into source GUI")
                h.send(sock, pane, "selector-admitted")
                gui.wait_for(lambda: "ACK:selector-admitted" in h.screen(sock, pane), "selector destination VTE input")
                original = current()["attachment"]["key"]
                original_surface = current()["view"]["surface"]
                h.pass_check("Move to selector explicitly transfers the same running execution between GUI windows")

                handle = gui.wait_for(lambda: find(app, Atspi.Role.IMAGE, "Drag running agent to a pane"), "native Agents drag handle")
                bounds = handle.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                start = connection.screen().root.translate_coords(left, bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
                target = gui.wait_for(lambda: find(other, Atspi.Role.PUSH_BUTTON, h.root.name), "destination shell tab")
                bounds = target.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                end = connection.screen().root.translate_coords(right, bounds.x + bounds.width // 2, bounds.y + 250)
                for cancel in (True, False):
                    xtest.fake_input(connection, gui.X.MotionNotify, x=start.x, y=start.y)
                    connection.sync()
                    time.sleep(.1)
                    xtest.fake_input(connection, gui.X.ButtonPress, 1)
                    for step in range(1, 41):
                        xtest.fake_input(connection, gui.X.MotionNotify,
                                         x=int(start.x + (end.x - start.x) * step / 40),
                                         y=int(start.y + (end.y - start.y) * step / 40))
                        connection.sync()
                        time.sleep(.025)
                    time.sleep(.2)
                    if cancel:
                        press("Escape")
                    xtest.fake_input(connection, gui.X.ButtonRelease, 1)
                    connection.sync()
                    if cancel:
                        time.sleep(.3)
                        source_input()
                        h.pass_check("Escape cancels a cross-window Agents drag without changing the original view or input owner")
                gui.wait_for(lambda: current()["view"] and current()["view"]["pane"] == destination, "native cross-window Agent drop")
                h.send(other_sock, destination, "native-cross-window")
                gui.wait_for(lambda: "ACK:native-cross-window" in h.screen(other_sock, destination), "native drop VTE input")
                preserves_execution()
                h.pass_check("Native Agents handle drag between GUI windows transfers the view while preserving exact PID and input")
                view = current()["view"]
                h.rpc(other_sock, "surface_close", pane=destination, surface=view["surface"])
                gui.wait_for(lambda: current()["attachment"] is None, "moved product tab detaches")
                preserves_execution()
                h.pass_check("Closing the cross-window moved Agent tab leaves the same execution alive in Background")
            finally:
                connection.close()
            h.close_window(other)
            h.close_window(app)
            return

        if args.replacement:
            opened = h.rpc(sock, "agent_view_open", pane=pane, instance=agent["instance"])
            original_surface = opened["surface_created"]["id"]
            for tab in tabs():
                if tab["id"] != original_surface:
                    h.rpc(sock, "surface_close", pane=pane, surface=tab["id"])
            assert [tab["id"] for tab in tabs()] == [original_surface]
            gui.wait_for(lambda: current()["view"], "source GUI location")
            original = current()["attachment"]["key"]
            other, other_sock = h.window("replacement-destination")
            workspace = h.rpc(other_sock, "workspace_create", root=str(h.root),
                              name="Replacement destination")["workspace_created"]["id"]
            h.rpc(other_sock, "workspace_focus", workspace=workspace)
            destination = h.workspace(other_sock, workspace)["panes"][0]["id"]

            def source_input():
                token = "source-" + uuid.uuid4().hex
                h.send(sock, pane, token)
                gui.wait_for(lambda: "ACK:" + token in h.screen(sock, pane), "fresh original input")
                value = current()
                assert value["attachment"]["key"] == original
                assert value["view"]["pane"] == pane
                preserves_execution()

            relay_command = [args.cli, "--socket", str(host_socket), "agent-relay", "--require-gui",
                             "--instance", agent["instance"], "--epoch", agent["epoch"],
                             "--replace-lease", original["lease"],
                             "--replace-generation", str(original["generation"])]
            for outcome in ("write-failure", "cancel", "timeout", "ready"):
                candidate_hold = h.root / (outcome + "-hold")
                candidate_writing = h.root / (outcome + "-writing")
                candidate_fail = h.root / (outcome + "-fail")
                candidate_pidfile = h.root / (outcome + "-pid")
                candidate_status = h.root / (outcome + "-status")
                candidate_hold.touch()
                command = shlex.join([
                    "env", "FIXTURE_HOLD=" + str(candidate_hold),
                    "FIXTURE_WRITING=" + str(candidate_writing), "FIXTURE_FAIL=" + str(candidate_fail),
                    "/bin/sh", "-c", "echo $$ > " + shlex.quote(str(candidate_pidfile))
                    + "; exec " + shlex.join(relay_command),
                ])
                h.send(other_sock, destination, command + "; code=$?; printf '%s' \"$code\" > "
                       + shlex.quote(str(candidate_status)))
                gui.wait_for(candidate_writing.exists, "replacement relay blocked on snapshot write")
                candidate_pid = int(candidate_pidfile.read_text())
                argv = Path(f"/proc/{candidate_pid}/cmdline").read_bytes().split(b"\0")
                assert b"agent-relay" in argv and b"--replace-lease" in argv, argv
                assert candidate_pid not in h.protected_pids and candidate_pid != pid
                source_input()
                if outcome == "cancel":
                    os.kill(candidate_pid, signal.SIGKILL)
                elif outcome == "write-failure":
                    candidate_fail.touch()
                elif outcome == "timeout":
                    deadline = time.monotonic() + 11
                    while time.monotonic() < deadline:
                        source_input()
                        time.sleep(0.2)
                candidate_hold.unlink()
                if outcome != "ready":
                    gui.wait_for(candidate_status.exists, "failed candidate returns to its terminal shell")
                    assert candidate_status.read_text() != "0"
                    gui.wait_for(lambda: not Path(f"/proc/{candidate_pid}").exists(), "candidate reaped")
                    source_input()
                    h.pass_check(f"Replacement relay {outcome} preserves original VTE input, location and exact execution")
                    continue
                gui.wait_for(lambda: current()["attachment"]["key"] != original
                             and current()["attachment"]["ready"], "new relay owns input after snapshot ACK")
                replacement = current()["attachment"]["key"]
                assert replacement["generation"] > original["generation"]
                assert current()["attachment"]["relay_pid"] == candidate_pid
                token = "destination-" + uuid.uuid4().hex
                h.send(other_sock, destination, token)
                gui.wait_for(lambda: "ACK:" + token in h.screen(other_sock, destination), "new VTE input")
                preserves_execution()
                h.pass_check("Replacement snapshot ACK gives the second window's real relay input on the same execution")
                gui.wait_for(lambda: all(tab["id"] != original_surface
                             for ws in h.tree(sock) for item in ws["panes"] for tab in item["tabs"]),
                             "committed transfer removes the old GUI tab")
                h.pass_check("Only a committed replacement removes the old GUI's sole tab without confirmation; failed candidates preserve it")
                def has_saved_binding(value):
                    if isinstance(value, dict):
                        binding = value.get("agent_binding")
                        return (isinstance(binding, dict) and binding.get("instance") == agent["instance"]
                                or any(has_saved_binding(item) for item in value.values()))
                    return isinstance(value, list) and any(has_saved_binding(item) for item in value)
                gui.wait_for(lambda: not has_saved_binding(json.loads(h.state_path.read_text())),
                             "previous GUI binding removed from saved layout")
                h.close_window(app)
                app, sock = h.window("replacement-source-restored")
                assert all(tab["id"] != original_surface
                           for ws in h.tree(sock) for item in ws["panes"] for tab in item["tabs"])
                assert current()["attachment"]["key"] == replacement
                preserves_execution()
                h.pass_check("The old GUI saves binding removal and reopening it cannot restore or steal the moved view")
                stale = subprocess.run(relay_command,
                                       env={**h.env, "FLOWMUX_SURFACE_ID": str(uuid.uuid4())},
                                       stdin=subprocess.DEVNULL,
                                       capture_output=True, text=True, timeout=5)
                assert stale.returncode != 0 and "explicit move required" in stale.stderr, stale
                assert current()["attachment"]["key"] == replacement
                preserves_execution()
                h.pass_check("Stale replacement lease is rejected without replacing or retrying the current owner")
                os.kill(candidate_pid, signal.SIGTERM)
                gui.wait_for(candidate_status.exists, "owning relay returns to terminal shell")
                assert candidate_status.read_text() == "0"
                gui.wait_for(lambda: current()["attachment"] is None, "new relay EOF detaches")
                preserves_execution()
                h.pass_check("Closing the new relay after transfer leaves the exact execution in Background")
            h.close_window(other)
            def restored_window_mapped():
                with gui.closing(gui.display.Display(h.env["DISPLAY"])) as connection:
                    atom = connection.intern_atom("_NET_WM_PID")
                    for window in connection.screen().root.query_tree().children:
                        owner = window.get_full_property(atom, gui.X.AnyPropertyType)
                        if owner is not None and int(owner.value[0]) == app.pid:
                            if window.get_attributes().map_state == gui.X.IsViewable:
                                return True
                return False
            gui.wait_for(restored_window_mapped, "restored GUI is mapped before its close request")
            h.close_window(app)
            return

        with ThreadPoolExecutor(max_workers=1) as pool:
            def pending_open():
                writing.unlink(missing_ok=True)
                hold.touch()
                future = pool.submit(h.rpc, sock, "agent_view_open", pane=pane, instance=agent["instance"])
                def waiting_for_write():
                    control.call("keep_alive")
                    return writing.exists()
                gui.wait_for(waiting_for_write, "relay trying to write actual initial snapshot")
                gui.wait_for(lambda: len(tabs()) == 2, "pending tab exists")
                assert not future.done(), "open reported success before snapshot write"
                attachment = current()["attachment"]
                assert attachment and not attachment["ready"] and current()["view"] is None
                gui.wait_for(lambda: label("Connecting to Agent…"), "visible Connecting status")
                preserves_execution()
                return future, tabs()[-1]["id"]

            future, surface = pending_open()
            h.pass_check("Snapshot write delay keeps open pending, location unpublished and Connecting visible")
            h.rpc(sock, "surface_close", pane=pane, surface=surface)
            try:
                future.result(timeout=5)
                raise AssertionError("closed destination returned success")
            except RuntimeError as error:
                assert "before connection" in str(error) or "disconnected" in str(error), error
            hold.unlink()
            gui.wait_for(lambda: current()["attachment"] is None, "closed relay detached")
            preserves_execution()
            h.pass_check("Close during snapshot wait remains responsive, rejects open and preserves the execution")

            future, surface = pending_open()
            try:
                future.result(timeout=15)
                raise AssertionError("unacknowledged snapshot returned success")
            except RuntimeError as error:
                assert "timed out" in str(error), error
            gui.wait_for(lambda: label("Agent connection timed out. Close this tab and reopen from Agents to retry."), "visible connection timeout")
            assert any(tab["id"] == surface for tab in tabs())
            preserves_execution()
            h.pass_check("Snapshot timeout returns an error and keeps the bound tab and execution without retrying")
            h.rpc(sock, "surface_close", pane=pane, surface=surface)
            hold.unlink()
            gui.wait_for(lambda: current()["attachment"] is None, "timed-out relay detached")

            future, surface = pending_open()
            fail.touch()
            hold.unlink()
            try:
                future.result(timeout=8)
                raise AssertionError("failed snapshot returned success")
            except RuntimeError as error:
                assert "disconnected" in str(error), error
            gui.wait_for(lambda: label("Agent connection failed. Reopen from Agents to retry."), "visible connection failure")
            assert any(tab["id"] == surface for tab in tabs()), "failure lost the binding"
            preserves_execution()
            h.pass_check("Failed snapshot reports an error, retains the tab binding and leaves the root process alive")

            fail.unlink()
            opened = h.rpc(sock, "agent_view_open", pane=pane, instance=agent["instance"])
            assert opened["surface_created"]["id"] == surface and len(tabs()) == 2
            assert current()["attachment"]["ready"]
            gui.wait_for(lambda: "READY-SNAPSHOT" in h.screen(sock, pane), "same execution snapshot")
            h.send(sock, pane, "fresh-after-recovery")
            gui.wait_for(lambda: "ACK:fresh-after-recovery" in h.screen(sock, pane), "fresh input after ready")
            preserves_execution()
            assert not label("Connecting to Agent…")
            h.pass_check("Reopen reconnects the same binding and PID, waits for ready ACK and accepts fresh input")
        h.close_window(app)
        preserves_execution()
        h.pass_check("GUI close after readiness recovery preserves the exact execution")
    finally:
        for path in h.root.glob("*-hold"):
            path.unlink(missing_ok=True)
        try:
            if control:
                try:
                    remaining = control.call("list")["agents"]
                except (OSError, AssertionError):
                    control.close()
                    control = host_fixture.HostClient(host_socket, "relay")
                    remaining = control.call("list")["agents"]
                for item in remaining:
                    if item["state"] == "running":
                        control.call("terminate", instance=item["instance"], epoch=item["epoch"], mode="terminate")
                        gui.wait_for(lambda: not Path(f"/proc/{item['pid']}").exists(), "fixture execution reaped", timeout=10)
        finally:
            if control:
                control.close()
            h.cleanup()


if __name__ == "__main__":
    main()
