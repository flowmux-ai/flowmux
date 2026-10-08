#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check document I/O isolation, edit ordering, and UI progress in a real GUI.

A one-shot fsync delay applies only to this test's editor document directory.
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
        library = h.root / "delay.so"
        subprocess.run(["cc", "-shared", "-fPIC", "-o", str(library),
                        str(Path(__file__).with_name("test-editor-io-delay.c")), "-ldl"], check=True)
        h.env.update(LD_PRELOAD=str(library), FM_ARCH_PROJECT=str(work) + "/",
                     FM_ARCH_DELAY_FLAG=str(flag), FM_ARCH_ENTERED=str(entered))
        h.start_display()
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

        document.unlink()
        os.mkfifo(document)
        time.sleep(1.5)
        try:
            latency = responsive(terminal)
            # Closing must fail safely while the ordered document flush cannot
            # finish. The error dialog remains interactive on the GTK thread.
            request_close()
            time.sleep(2.5)
            assert process.poll() is None, "window closed before pending document I/O finished"
            # WM close dismisses the modal without depending on WebKit keyboard
            # focus in bare Xvfb. Allow the libadwaita close animation to finish.
            request_close()
            time.sleep(0.5)
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
        h.close_window(process)
        h.pass_check(f"blocked FIFO read: GTK responds in {latency:.4f}s; close waits safely and succeeds after release")
        connection.close()
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
