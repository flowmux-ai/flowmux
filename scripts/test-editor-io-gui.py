#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check document I/O isolation, edit ordering, and UI progress in a real GUI.

A one-shot fsync delay applies only to this test's document and recovery directories.
The FIFO case uses a real filesystem object with no injected read functions.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import threading
import time

from Xlib import X, XK, display, protocol
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
    try:
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell)}))
        work = h.root / "project"
        work.mkdir()
        document = work / "slow.txt"
        document.write_text("Original regular file\n")
        flag, entered = h.root / "delay-save", h.root / "fsync-entered"
        recovery_flag, recovery_entered = h.root / "delay-recovery", h.root / "recovery-entered"
        library = h.root / "delay.so"
        subprocess.run(["cc", "-shared", "-fPIC", "-o", str(library),
                        str(Path(__file__).with_name("test-editor-io-delay.c")), "-ldl"], check=True)
        h.env.update(LD_PRELOAD=str(library), FM_ARCH_PROJECT=str(work) + "/",
                     FM_ARCH_DELAY_FLAG=str(flag), FM_ARCH_ENTERED=str(entered),
                     FM_ARCH_RECOVERY=str(h.root / "state/flowmux/editor-recovery") + "/",
                     FM_ARCH_RECOVERY_FLAG=str(recovery_flag),
                     FM_ARCH_RECOVERY_ENTERED=str(recovery_entered))
        h.env["GTK_A11Y"] = "atspi"
        h.start_display()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi

        process, path = h.window("editor-io")
        workspace = h.rpc(path, "workspace_create", name="Editor I/O", root=str(work))["workspace_created"]["id"]
        h.rpc(path, "workspace_focus", workspace=workspace)
        pane = h.workspace(path, workspace)["panes"][0]["id"]
        connection = display.Display(h.env["DISPLAY"])

        def focus_window():
            for window in connection.screen().root.query_tree().children:
                pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                if pid is not None and int(pid.value[0]) == process.pid and window.get_attributes().map_state == X.IsViewable:
                    window.set_input_focus(X.RevertToParent, X.CurrentTime)
                    connection.sync()
                    return True

        def key(name, modifiers=(), delay=0.15):
            codes = [connection.keysym_to_keycode(XK.string_to_keysym(k)) for k in (*modifiers, name)]
            assert all(codes)
            for code in codes:
                xtest.fake_input(connection, X.KeyPress, code)
            for code in reversed(codes):
                xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()
            time.sleep(delay)

        def responsive(terminal):
            start = time.monotonic()
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(1)
                stream.connect(str(path))
                stream.sendall((json.dumps({"id":1,"kind":"request","verb":"pane_read_screen","pane":terminal}) + "\n").encode())
                reply = json.loads(stream.makefile("rb").readline())
            assert "screen_contents" in reply, reply
            return time.monotonic() - start

        def request_close():
            for window in connection.screen().root.query_tree().children:
                pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                if pid is not None and int(pid.value[0]) == process.pid:
                    window.send_event(protocol.event.ClientMessage(
                        window=window, client_type=connection.intern_atom("WM_PROTOCOLS"),
                        data=(32, [connection.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0]),
                    ))
            connection.sync()

        def sync_error_visible():
            def walk(node):
                yield node
                for index in range(node.get_child_count()):
                    child = node.get_child_at_index(index)
                    if child:
                        yield from walk(child)
            return any(node.get_name() == "Could not synchronize changes"
                       and node.get_state_set().contains(Atspi.StateType.SHOWING)
                       for node in walk(Atspi.get_desktop(0)))

        gui.wait_for(focus_window, "mapped editor test window")
        time.sleep(0.5)
        key("f", ("Control_L", "Alt_L"))
        key("Right", ("Alt_L",))
        key("Return")
        editor = gui.wait_for(lambda: next((t for w in h.tree(path) for p in w["panes"]
                                           for t in p["tabs"] if t["kind"] == "editor"), None), "editor tab")
        time.sleep(2)
        terminal = h.rpc(path, "pane_split", pane=pane, direction="vertical")["pane_split_done"]["new_pane"]
        h.rpc(path, "surface_focus", pane=pane, surface=editor["id"])
        time.sleep(0.3)
        key("End", ("Control_L",))
        key("x")
        key("s", ("Control_L",))
        gui.wait_for(lambda: document.read_text().endswith("x"), "ordinary document save")
        key("y")
        flag.touch()
        key("s", ("Control_L",), delay=0.01)
        gui.wait_for(entered.exists, "delayed document fsync")
        receipt = entered.read_text()
        tid = int(re.search(r"tid=(\d+)", receipt)[1])
        assert tid != process.pid, "document fsync ran on the GTK thread"
        latency = responsive(terminal)
        key("z")  # Edit while the older version is still being written.
        gui.wait_for(lambda: document.read_text().endswith("xy"), "first save completes in order")
        key("s", ("Control_L",))
        gui.wait_for(lambda: document.read_text().endswith("xyz"), "edit during save remains dirty and saves next")
        h.pass_check(f"3-second document save: GTK responds in {latency:.4f}s; concurrent edit is preserved")

        original = document.read_bytes()
        def rss_kib():
            status = Path(f"/proc/{process.pid}/status").read_text()
            return int(re.search(r"VmRSS:\s+(\d+)", status)[1])
        before = rss_kib()
        with document.open("r+b") as growing:
            growing.truncate(1024 ** 3)
        time.sleep(2)
        latency = responsive(terminal)
        assert document.stat().st_size == 1024 ** 3
        growth = rss_kib() - before
        assert growth < 64 * 1024, f"external file consumed {growth} KiB"
        document.write_bytes(original)
        time.sleep(1)
        h.pass_check(f"1 GiB external file: GTK responds in {latency:.4f}s; RSS change {growth} KiB; disk preserved")

        document.unlink()
        os.mkfifo(document)
        time.sleep(1.5)
        try:
            latency = responsive(terminal)
            # Closing must fail safely while the ordered document flush cannot
            # finish. The error dialog remains interactive on the GTK thread.
            request_close()
            gui.wait_for(sync_error_visible, "blocked document synchronization error")
            assert process.poll() is None, "window closed before pending document I/O finished"
            # WM close dismisses the modal without depending on WebKit keyboard
            # focus in bare Xvfb. Sending it before the dialog appears only hits
            # the already pending window-close request and leaves the modal open.
            request_close()
            gui.wait_for(lambda: not sync_error_visible(), "synchronization error dismissed")
            responsive(terminal)
        finally:
            def release():
                with document.open("w") as writer:
                    writer.write("FIFO read completed\n")
            writer = threading.Thread(target=release, daemon=True)
            writer.start()
            writer.join(5)
        assert not writer.is_alive(), "background reader never consumed the FIFO"
        document.unlink()
        document.write_text("Restored regular file\n")
        responsive(terminal)
        h.pass_check(f"blocked FIFO read: GTK responds in {latency:.4f}s; close waits safely for pending edits")

        h.rpc(path, "surface_focus", pane=pane, surface=editor["id"])
        time.sleep(1.5)
        key("End", ("Control_L",))
        recovery_flag.touch()
        key("x", delay=.01)
        gui.wait_for(recovery_entered.exists, "delayed recovery fsync")
        key("s", ("Control_L",), delay=.01)
        gui.wait_for(lambda: document.read_text().endswith("x"), "save during delayed recovery write")
        h.rpc(path, "surface_close", pane=pane, surface=editor["id"])
        latency = responsive(terminal)
        recovery_root = h.root / "state/flowmux/editor-recovery"
        gui.wait_for(lambda: not list(recovery_root.rglob("*.json"))
                     and not list(recovery_root.rglob(".tmp*")), "closed editor recovery operations drained")
        h.pass_check(f"3-second recovery fsync: closed-tab GTK responds in {latency:.4f}s; saved snapshot is removed")

        # Reopen to cover closing the last window with an editor still registered.
        gui.wait_for(focus_window, "mapped editor test window")
        h.rpc(path, "pane_focus", pane=terminal)
        time.sleep(.3)
        key("Right", ("Alt_L",))
        key("Return")
        pane, editor = gui.wait_for(lambda: next(((p["id"], t) for w in h.tree(path) for p in w["panes"]
                                                for t in p["tabs"] if t["kind"] == "editor"), None), "reopened editor")
        h.rpc(path, "surface_focus", pane=pane, surface=editor["id"])
        time.sleep(2)
        key("End", ("Control_L",))
        recovery_entered.unlink()
        recovery_flag.touch()
        key("y", delay=.01)
        gui.wait_for(recovery_entered.exists, "delayed recovery write during window close")
        key("s", ("Control_L",), delay=.01)
        gui.wait_for(lambda: document.read_text().endswith("xy"), "save before window close")
        # The last window may disappear, but the process must retain the worker
        # until the queued removal after this successful save reaches disk.
        request_close()
        time.sleep(.2)
        assert process.poll() is None, "process exited before the stalled recovery writer drained"
        process.wait(timeout=15)
        assert process.returncode == 0, process.returncode
        assert not list(recovery_root.rglob("*.json")), "saved document left a stale recovery snapshot"
        assert not list(recovery_root.rglob(".tmp*")), "recovery write was abandoned during exit"
        h.pass_check("last-window close drains the delayed recovery snapshot and its queued removal before exit")
        connection.close()
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
