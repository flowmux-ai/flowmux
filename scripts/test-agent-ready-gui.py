#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Delay actual relay snapshot writes; verify readiness, cancellation and recovery."""

from agent_host_protocol import SOCKET_NAME

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
#include <sys/socket.h>
#include <unistd.h>
static _Atomic int ack_fd = -1, ack_stage;
int rename(const char *oldpath, const char *newpath) {
    static int (*real_rename)(const char *, const char *);
    if (!real_rename) real_rename = dlsym(RTLD_NEXT, "rename");
    const char *block = getenv("FIXTURE_BLOCK_STATE");
    const char *state = getenv("FIXTURE_STATE_PATH");
    if (block && state && !strcmp(newpath, state) && !access(block, F_OK)) {
        const char *denied = getenv("FIXTURE_STATE_DENIED");
        int marker = denied ? open(denied, O_WRONLY | O_CREAT, 0600) : -1;
        if (marker >= 0) close(marker);
        errno = EACCES;
        return -1;
    }
    return real_rename(oldpath, newpath);
}
static void observe_write(int fd, const void *data, size_t count) {
    const char *hold = getenv("FIXTURE_HOLD_ADMISSION");
    const char gui_op[] = "\"op\":\"gui_view_ready\"";
    if (hold && !access(hold, F_OK) && memmem(data, count, gui_op, sizeof(gui_op) - 1)) {
        int marker = open(getenv("FIXTURE_ADMITTING"), O_WRONLY | O_CREAT, 0600);
        if (marker >= 0) close(marker);
        while (!access(hold, F_OK)) usleep(10000);
    }
    const char *marker = getenv("FIXTURE_FRAGMENT_ACK");
    const char op[] = "\"op\":\"view_ready\"";
    if (marker && *marker && memmem(data, count, op, sizeof(op) - 1)) {
        ack_fd = fd;
        ack_stage = 1;
    }
}
static int fragment_read(int fd, size_t *count) {
    if (fd != ack_fd) return 0;
    if (ack_stage == 1) {
        if (*count > 8) *count = 8;
    } else if (ack_stage == 2) {
        ack_stage = 3;
        int marker = open(getenv("FIXTURE_FRAGMENT_ACK"), O_WRONLY | O_CREAT, 0600);
        if (marker >= 0) close(marker);
        errno = EAGAIN;
        return -1;
    }
    return 0;
}
ssize_t read(int fd, void *data, size_t count) {
    static ssize_t (*real_read)(int, void *, size_t);
    if (!real_read) real_read = dlsym(RTLD_NEXT, "read");
    if (fragment_read(fd, &count)) return -1;
    ssize_t result = real_read(fd, data, count);
    if (fd == ack_fd && ack_stage == 1 && result > 0) ack_stage = 2;
    return result;
}
ssize_t recv(int fd, void *data, size_t count, int flags) {
    static ssize_t (*real_recv)(int, void *, size_t, int);
    if (!real_recv) real_recv = dlsym(RTLD_NEXT, "recv");
    if (fragment_read(fd, &count)) return -1;
    ssize_t result = real_recv(fd, data, count, flags);
    if (fd == ack_fd && ack_stage == 1 && result > 0) ack_stage = 2;
    return result;
}
ssize_t send(int fd, const void *data, size_t count, int flags) {
    static ssize_t (*real_send)(int, const void *, size_t, int);
    if (!real_send) real_send = dlsym(RTLD_NEXT, "send");
    observe_write(fd, data, count);
    return real_send(fd, data, count, flags);
}
ssize_t write(int fd, const void *data, size_t count) {
    static ssize_t (*real_write)(int, const void *, size_t);
    static int relay;
    if (!real_write) real_write = dlsym(RTLD_NEXT, "write");
    observe_write(fd, data, count);
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
        admission_hold, admitting = h.root / "admission-hold", h.root / "admitting"
        h.env.update(GTK_A11Y="atspi", LD_PRELOAD=str(preload), FIXTURE_HOLD=str(hold),
                     FIXTURE_WRITING=str(writing), FIXTURE_FAIL=str(fail),
                     FIXTURE_HOLD_ADMISSION=str(admission_hold), FIXTURE_ADMITTING=str(admitting))
        options = h.root / "config/flowmux/options.json"
        options.parent.mkdir(parents=True, exist_ok=True)
        options.write_text(json.dumps({"default_shell": "/bin/sh"}))
        h.start_display()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ.pop("AT_SPI_BUS_ADDRESS", None)
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi, GLib
        Atspi.set_timeout(3000, 3000)
        host_socket = h.root / "run" / SOCKET_NAME
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
        source_state_block, source_state_denied = h.root / "source-state-block", h.root / "source-state-denied"
        h.env.update(FIXTURE_BLOCK_STATE=str(source_state_block), FIXTURE_STATE_DENIED=str(source_state_denied),
                     FIXTURE_STATE_PATH=str(h.root / "state/flowmux/state.json"))
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
            state_block, state_denied = h.root / "state-block", h.root / "state-denied"
            state_file = h.root / "state/flowmux/state.json"
            # Only this GUI's regular state saves fail; Host checkpoints must still be durable.
            h.env.update(FIXTURE_BLOCK_STATE=str(state_block), FIXTURE_STATE_DENIED=str(state_denied),
                         FIXTURE_STATE_PATH=str(state_file))
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
                        gui.wait_for(lambda: source_input() or future.done(),
                                     "product move reaches its connection deadline", timeout=40)
                        try:
                            future.result(timeout=40)
                            raise AssertionError("unacknowledged product move returned success")
                        except RuntimeError as error:
                            assert "timed out" in str(error), error
                        source_input()
                    if outcome == "ready":
                        deadline = time.monotonic() + 11
                        while time.monotonic() < deadline:
                            source_input()
                            time.sleep(0.2)
                        assert not future.done(), "delayed move completed before snapshot write"
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
                        try:
                            node.clear_cache()
                            state = node.get_state_set()
                            name, role = node.get_name(), node.get_role()
                            count = node.get_child_count()
                        except GLib.Error as error:
                            # A GTK row can disappear between enumerating it and querying it.
                            if "Object does not exist" in str(error):
                                return
                            raise
                        if state.contains(Atspi.StateType.SHOWING):
                            yield name, role, node
                        for index in range(count):
                            child = node.get_child_at_index(index)
                            if child:
                                yield from walk(child)
                    desktop = Atspi.get_desktop(0)
                    desktop.clear_cache()
                    node = next((desktop.get_child_at_index(i) for i in range(desktop.get_child_count())
                                 if desktop.get_child_at_index(i).get_process_id() == process.pid), None)
                    return list(walk(node)) if node else []

                def find(process, role, name):
                    return next((node for item_name, item_role, node in nodes(process)
                                 if item_role == role and item_name == name), None)

                def action(process, role, name):
                    node = gui.wait_for(lambda: find(process, role, name), name)
                    assert node.get_action_iface().do_action(0)

                def press(name):
                    key = connection.keysym_to_keycode(XK.string_to_keysym(name))
                    xtest.fake_input(connection, gui.X.KeyPress, key)
                    xtest.fake_input(connection, gui.X.KeyRelease, key)
                    connection.sync()
                    time.sleep(.08)

                before = current()
                pending_tabs = {tab["id"] for tab in tabs()}
                source_state_block.touch()
                left.set_input_focus(gui.X.RevertToParent, gui.X.CurrentTime)
                for key in ("Control_L", "Shift_L"):
                    xtest.fake_input(connection, gui.X.KeyPress, connection.keysym_to_keycode(XK.string_to_keysym(key)))
                press("t")
                for key in ("Shift_L", "Control_L"):
                    xtest.fake_input(connection, gui.X.KeyRelease, connection.keysym_to_keycode(XK.string_to_keysym(key)))
                connection.sync()
                extra = gui.wait_for(lambda: next((tab["id"] for tab in tabs() if tab["id"] not in pending_tabs), None),
                                     "native new tab before admission")
                gui.wait_for(source_state_denied.exists, "native tab save delayed until after preparation")
                admission_hold.touch()
                with ThreadPoolExecutor(max_workers=1) as pool:
                    future = pool.submit(h.rpc, sock, "agent_view_open", pane=pane, instance=agent["instance"])
                    try:
                        gui.wait_for(admitting.exists, "GUI checkpoint prepared before readiness request")
                        candidate = next(tab["id"] for tab in tabs() if tab["title"] == agent["label"])
                        source_state_block.unlink()
                        gui.wait_for(lambda: extra in state_file.read_text(), "newer destination layout persisted")
                    finally:
                        admission_hold.unlink(missing_ok=True)
                        source_state_block.unlink(missing_ok=True)
                    # The Host refuses the checkpoint taken before that save. The window offers
                    # its current layout and the move completes, unless the refusal comes too late.
                    try:
                        moved = future.result(timeout=12)["surface_created"]["id"] == candidate
                        assert moved
                    except RuntimeError as error:
                        assert any(text in str(error) for text in ("before connection", "disconnected", "timed out")), error
                        moved = False
                assert extra in state_file.read_text(), "stale checkpoint overwrote a newer window save"
                if moved:
                    gui.wait_for(lambda: current()["view"] and current()["view"]["pane"] == pane,
                                 "view admitted with a checkpoint of the newer layout")
                    h.send(sock, pane, "after-concurrent-save")
                    gui.wait_for(lambda: "ACK:after-concurrent-save" in h.screen(sock, pane), "input after admission with the newer layout")
                else:
                    assert current()["attachment"]["key"] == before["attachment"]["key"]
                    h.send(other_sock, destination, "after-concurrent-save")
                    gui.wait_for(lambda: "ACK:after-concurrent-save" in h.screen(other_sock, destination), "original input after rejected stale checkpoint")
                    h.rpc(sock, "surface_close", pane=pane, surface=candidate)
                preserves_execution()
                h.pass_check("A save completing after checkpoint preparation is never overwritten by the stale checkpoint; the native-created tab, the execution and fresh input are preserved")

                left.set_input_focus(gui.X.RevertToParent, gui.X.CurrentTime)
                # An open view has no destination menu item; an explicit open moves it here.
                h.rpc(sock, "agent_view_open", pane=pane, instance=agent["instance"])
                gui.wait_for(lambda: current()["view"] and current()["view"]["pane"] == pane, "explicit open moves into source GUI")
                h.send(sock, pane, "selector-admitted")
                gui.wait_for(lambda: "ACK:selector-admitted" in h.screen(sock, pane), "selector destination VTE input")
                original = current()["attachment"]["key"]
                original_surface = current()["view"]["surface"]
                h.pass_check("An explicit open transfers the same running execution between GUI windows")

                def agent_row(process):
                    return next((node for _, role, node in nodes(process) if role == Atspi.Role.PUSH_BUTTON
                                 and "Open the same running agent" in (node.get_description() or "")), None)

                handle = gui.wait_for(lambda: agent_row(app), "draggable Agents row")
                bounds = handle.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                start = connection.screen().root.translate_coords(left, bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
                target = gui.wait_for(lambda: find(other, Atspi.Role.PUSH_BUTTON, h.root.name), "destination shell tab")
                bounds = target.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                end = connection.screen().root.translate_coords(right, bounds.x + bounds.width // 2, bounds.y + 250)
                def drag(start, end, cancel=False):
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
                drag(start, end, cancel=True)
                time.sleep(.3)
                source_input()
                h.pass_check("Escape cancels a cross-window Agents drag without changing the original view or input owner")
                drag(start, end)
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

                handle = gui.wait_for(lambda: agent_row(app), "source Background Agent row")
                bounds = handle.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                start = connection.screen().root.translate_coords(left, bounds.x + bounds.width // 2,
                                                                  bounds.y + bounds.height // 2)
                drag(start, end)
                view = gui.wait_for(lambda: (view if (view := current()["view"]) and view["pane"] == destination else None),
                                    "second native Agent drop into the same window")
                h.send(other_sock, destination, "second-cross-window")
                gui.wait_for(lambda: "ACK:second-cross-window" in h.screen(other_sock, destination), "second drop input")
                preserves_execution()
                h.rpc(other_sock, "surface_close", pane=destination, surface=view["surface"])
                gui.wait_for(lambda: current()["attachment"] is None, "second moved tab detaches")
                h.pass_check("A second external Agents drop works without a local drag in the destination window")

                opened = h.rpc(sock, "agent_view_open", pane=pane, instance=agent["instance"])
                source_surface = opened["surface_created"]["id"]
                gui.wait_for(lambda: current()["view"] and current()["view"]["surface"] == source_surface,
                             "reattached source before tab strip drag")
                source = gui.wait_for(lambda: find(app, Atspi.Role.PUSH_BUTTON, agent["label"]), "source Agent tab")
                bounds = source.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                start = connection.screen().root.translate_coords(left, bounds.x + bounds.width // 2,
                                                                  bounds.y + bounds.height // 2)
                target = gui.wait_for(lambda: find(other, Atspi.Role.PUSH_BUTTON, h.root.name), "destination tab anchor")
                bounds = target.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                anchor = connection.screen().root.translate_coords(right, bounds.x + bounds.width // 4,
                                                                   bounds.y + bounds.height // 2)
                left.set_input_focus(gui.X.RevertToParent, gui.X.CurrentTime)
                drag(start, anchor)
                view = gui.wait_for(lambda: (view if (view := current()["view"]) and view["pane"] == destination else None),
                                    "native Agent tab moved to the existing destination window")
                assert [tab["id"] for tab in h.workspace(other_sock, other_workspace)["panes"][0]["tabs"]][0] == view["surface"]
                gui.wait_for(lambda: all(tab["id"] != source_surface for tab in tabs()), "source tab removed")
                h.send(other_sock, destination, "strip-cross-window")
                gui.wait_for(lambda: "ACK:strip-cross-window" in h.screen(other_sock, destination), "tab strip move input")
                preserves_execution()
                h.rpc(other_sock, "surface_close", pane=destination, surface=view["surface"])
                gui.wait_for(lambda: current()["attachment"] is None, "strip-moved tab detaches")
                h.pass_check("Native Agent tab strip drop honors the anchor and preserves the running PID across processes")

                # A connection wait must not delete a new workspace or the user's new tabs.
                state_block.touch()
                writing.unlink(missing_ok=True)
                hold.touch()
                right.set_input_focus(gui.X.RevertToParent, gui.X.CurrentTime)
                # Clicking a Background agent's row asks where to open it.
                assert gui.wait_for(lambda: agent_row(other), "Background Agent row").get_action_iface().do_action(0)
                picker = gui.wait_for(lambda: find(other, Atspi.Role.COMBO_BOX, "Agent destination"), "new workspace picker")
                bounds = picker.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                point = connection.screen().root.translate_coords(right, bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
                xtest.fake_input(connection, gui.X.MotionNotify, x=point.x, y=point.y)
                xtest.fake_input(connection, gui.X.ButtonPress, 1)
                xtest.fake_input(connection, gui.X.ButtonRelease, 1)
                connection.sync()
                press("End")
                press("Return")
                before = {ws["id"] for ws in h.tree(other_sock)}
                action(other, Atspi.Role.PUSH_BUTTON, "Open agent")
                created = gui.wait_for(lambda: next((ws for ws in h.tree(other_sock) if ws["id"] not in before), None), "new workspace before connection")
                gui.wait_for(lambda: writing.exists() and writing.read_text().strip().isdigit(), "new workspace snapshot held")
                created_pane = created["panes"][0]["id"]
                extra = h.rpc(other_sock, "surface_create", workspace=created["id"], cwd=str(h.root))["surface_created"]["id"]
                gui.wait_for(lambda: preserves_execution() or find(other, Atspi.Role.LABEL, "Agent view connection timed out. Close this tab and reopen it from Agents to retry."), "new workspace connection timeout", timeout=40)
                retained = h.workspace(other_sock, created["id"])
                retained_tabs = [tab for item in retained["panes"] for tab in item["tabs"]]
                assert any(tab["id"] == extra for tab in retained_tabs), retained
                candidate = next(tab["id"] for tab in retained_tabs if tab["title"] == agent["label"])
                preserves_execution()
                action(other, Atspi.Role.PUSH_BUTTON, "OK")
                hold.unlink()
                candidate_pid = int(writing.read_text())
                gui.wait_for(lambda: not Path(f"/proc/{candidate_pid}").exists(), "timed-out new workspace relay reaped")
                opened = h.rpc(other_sock, "agent_view_open", pane=created_pane, instance=agent["instance"])
                assert opened["surface_created"]["id"] == candidate
                h.send(other_sock, created_pane, "new-workspace-recovered")
                gui.wait_for(lambda: "ACK:new-workspace-recovered" in h.screen(other_sock, created_pane), "retained workspace input")
                assert any(tab["id"] == extra for item in h.workspace(other_sock, created["id"])["panes"] for tab in item["tabs"])
                preserves_execution()
                h.pass_check("New workspace timeout preserves its Agent binding and user-added tab; reopening accepts input with the same PID")
                gui.wait_for(state_denied.exists, "destination GUI state write denied")
                raw = state_file.read_text()
                assert candidate in raw and extra in raw and created["id"] in raw, raw
                assert other in h.children and other.pid not in h.protected_pids
                other.kill()
                other.wait(timeout=10)
                gui.wait_for(lambda: current()["attachment"] is None, "crashed destination detached")
                preserves_execution()
                state_block.unlink()
                other, other_sock = h.window("gui-crash-restored")
                gui.wait_for(lambda: current()["view"] and current()["view"]["pane"] == created_pane,
                             "crashed GUI destination restored from Host checkpoint")
                restored = h.workspace(other_sock, created["id"])
                assert {candidate, extra}.issubset({tab["id"] for item in restored["panes"] for tab in item["tabs"]})
                h.send(other_sock, created_pane, "after-destination-crash")
                gui.wait_for(lambda: "ACK:after-destination-crash" in h.screen(other_sock, created_pane), "restored destination input")
                preserves_execution()
                gui.wait_for(lambda: window(other), "restored native window mapped")
                h.pass_check("With GUI saves denied, Host admission persists the exact destination and user tab across SIGKILL and restores same-PID input")

                left.set_input_focus(gui.X.RevertToParent, gui.X.CurrentTime)
                action(app, Atspi.Role.PUSH_BUTTON, "Terminate agent")
                gui.wait_for(lambda: not host_socket.exists(), "Host retires after its final execution ends")
                gui.wait_for(lambda: all(agent_row(process) is None for process in (app, other)),
                             "the ended execution leaves Agents in both windows")
                assert not host_socket.exists()
                h.pass_check("A retired Host's last execution leaves Agents in both windows without restarting the Host")
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
                fragmented_ack = h.root / (outcome + "-fragmented-ack")
                candidate_hold.touch()
                command = shlex.join([
                    "env", "FIXTURE_HOLD=" + str(candidate_hold),
                    "FIXTURE_WRITING=" + str(candidate_writing), "FIXTURE_FAIL=" + str(candidate_fail),
                    "FIXTURE_FRAGMENT_ACK=" + (str(fragmented_ack) if outcome == "ready" else ""),
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
                elif outcome in ("timeout", "ready"):
                    # A supported large snapshot can take more than the old 10s limit.
                    deadline = time.monotonic() + (31 if outcome == "timeout" else 11)
                    while time.monotonic() < deadline:
                        source_input()
                        time.sleep(0.2)
                released = time.monotonic()
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
                attachment = current()["attachment"]
                assert attachment is not None, ("ready relay detached", h.screen(other_sock, destination))
                replacement = attachment["key"]
                gui.wait_for(fragmented_ack.exists, "actual relay consumed partial readiness ACK")
                assert replacement["generation"] > original["generation"]
                assert attachment["relay_pid"] == candidate_pid
                token = "destination-" + uuid.uuid4().hex
                h.send(other_sock, destination, token)
                gui.wait_for(lambda: "ACK:" + token in h.screen(other_sock, destination), "new VTE input")
                assert time.monotonic() - released >= 2.8, "readiness timeout was not exercised"
                preserves_execution()
                h.pass_check("Replacement snapshot ACK gives the second window's real relay input on the same execution")
                h.pass_check("Partial readiness ACK times out, reconciles the exact lease and preserves fresh VTE input")
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
            gui.wait_for(lambda: preserves_execution() or future.done(),
                         "snapshot open reaches its connection deadline", timeout=40)
            try:
                future.result(timeout=40)
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
            future, reopened_surface = pending_open()
            time.sleep(11)
            assert not future.done(), "delayed open completed before snapshot write"
            h.rpc(sock, "ping")
            hold.unlink()
            opened = future.result(timeout=15)
            assert opened["surface_created"]["id"] == surface == reopened_surface and len(tabs()) == 2
            assert current()["attachment"]["ready"]
            gui.wait_for(lambda: "READY-SNAPSHOT" in h.screen(sock, pane), "same execution snapshot")
            h.send(sock, pane, "fresh-after-recovery")
            gui.wait_for(lambda: "ACK:fresh-after-recovery" in h.screen(sock, pane), "fresh input after ready")
            preserves_execution()
            assert not label("Connecting to Agent…")
            h.pass_check("Reopen delayed beyond 10s reconnects the same binding and PID, waits for ready ACK and accepts fresh input")
        h.close_window(app)
        preserves_execution()
        h.pass_check("GUI close after readiness recovery preserves the exact execution")
    finally:
        for path in h.root.glob("*-hold"):
            path.unlink(missing_ok=True)
        try:
            if control and host_socket.exists():
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
