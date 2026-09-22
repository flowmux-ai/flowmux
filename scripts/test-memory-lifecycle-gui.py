#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check file-dialog and download lifetimes in an isolated running flowmux.

Run with: uv run --with python-xlib scripts/test-memory-lifecycle-gui.py
Uses the SSH GUI harness only for isolated Xvfb/D-Bus/XDG setup and IPC.
"""

import argparse
from contextlib import closing
from http.server import BaseHTTPRequestHandler, HTTPServer
import importlib.util
import json
from pathlib import Path
import signal
import sys
import threading
import time

from Xlib import X, XK, display
from Xlib.ext import xtest


sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "gui_harness", Path(__file__).with_name("test-ssh-workspace-gui.py")
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
    harness = gui.Harness(args)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        files = harness.root / "files"
        files.mkdir()
        (files / "old.txt").write_text("rename fixture\n")
        downloads = harness.root / "downloads"
        downloads.mkdir()
        (harness.root / "config/user-dirs.dirs").write_text(
            f'XDG_DOWNLOAD_DIR="{downloads}"\n'
        )
        harness.start_display()
        process, socket = harness.window("memory-lifecycle")
        workspace = harness.rpc(
            socket, "workspace_create", name="Memory lifecycle", root=str(files)
        )["workspace_created"]["id"]
        harness.rpc(socket, "workspace_focus", workspace=workspace)
        pane = harness.workspace(socket, workspace)["panes"][0]["id"]
        harness.rpc(socket, "pane_focus", pane=pane)

        with closing(display.Display(harness.env["DISPLAY"])) as connection:
            pid_atom = connection.intern_atom("_NET_WM_PID")

            def windows(title=None):
                found = []
                for window in connection.screen().root.query_tree().children:
                    pid = window.get_full_property(pid_atom, X.AnyPropertyType)
                    if pid is None or int(pid.value[0]) != process.pid:
                        continue
                    if window.get_attributes().map_state != X.IsViewable:
                        continue
                    if title is None or window.get_wm_name() == title:
                        found.append(window)
                return found

            def focus(window):
                window.set_input_focus(X.RevertToParent, X.CurrentTime)
                connection.sync()

            def keys(*names):
                codes = [connection.keysym_to_keycode(XK.string_to_keysym(name)) for name in names]
                assert all(codes), names
                for code in codes:
                    xtest.fake_input(connection, X.KeyPress, code)
                for code in reversed(codes):
                    xtest.fake_input(connection, X.KeyRelease, code)
                connection.sync()
                time.sleep(0.1)

            main_window = gui.wait_for(lambda: windows(), "main window")[0]
            focus(main_window)
            time.sleep(0.5)
            keys("Control_L", "Alt_L", "f")
            keys("Alt_L", "Right")
            for _ in range(4):
                focus(main_window)
                keys("F2")
                popup = gui.wait_for(lambda: windows("Rename"), "Rename popup")[0]
                focus(popup)
                keys("Escape")
                gui.wait_for(lambda: not windows("Rename"), "Rename popup closed")
                assert (files / "old.txt").exists()
            focus(main_window)
            keys("F2")
            popup = gui.wait_for(lambda: windows("Rename"), "Rename popup")[0]
            focus(popup)
            keys("Control_L", "a")
            for character in "new.txt":
                keys("period" if character == "." else character)
            keys("Return")
            gui.wait_for(lambda: (files / "new.txt").exists(), "file renamed by Enter")
            assert not (files / "old.txt").exists()
            gui.wait_for(lambda: not windows("Rename"), "Rename popup closed after success")
            harness.pass_check("four Rename/Escape cycles and Rename/Enter preserve file behavior and close dialogs")

        payload = b"flowmux download lifecycle\n"

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                if self.path == "/download":
                    self.send_header("Content-Type", "application/octet-stream")
                    self.send_header("Content-Disposition", 'attachment; filename="lifecycle.txt"')
                    body = payload
                else:
                    self.send_header("Content-Type", "text/html")
                    body = b'<a href="/download">Download fixture</a>'
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, *_):
                pass

        with HTTPServer(("127.0.0.1", 0), Handler) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                browser = harness.rpc(
                    socket, "browser_open", target_pane=pane, direction="vertical",
                    url=f"http://127.0.0.1:{server.server_port}/",
                )["browser_pane_opened"]["pane"]

                def download_ref():
                    snapshot = harness.rpc(socket, "browser_snapshot", pane=browser)["browser_result"]["value"]
                    if isinstance(snapshot, str):
                        snapshot = json.loads(snapshot)
                    return next((key for key, value in snapshot["refs"].items()
                                 if value.get("name") == "Download fixture"), None)

                for count in range(1, 4):
                    target = gui.wait_for(download_ref, "download link")
                    harness.rpc(socket, "browser_click", pane=browser, target=target)
                    gui.wait_for(lambda: len(list(downloads.glob("lifecycle*.txt"))) == count,
                                 "download saved by browser tab")
                assert all(path.read_bytes() == payload for path in downloads.glob("lifecycle*.txt"))
                harness.rpc(socket, "pane_close", pane=browser)
                harness.pass_check("live browser saves three downloads with correct contents and closes normally")
            finally:
                server.shutdown()
                thread.join(timeout=5)
        harness.close_window(process)
    finally:
        harness.cleanup()


if __name__ == "__main__":
    main()
