#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify degraded startup, explicit close, and normal session restoration.

Uses the shared isolated Xvfb/D-Bus/XDG harness; never closes user windows.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import sys
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
        h.start_display()
        connection = display.Display(h.env["DISPLAY"])

        def window(process):
            for w in connection.screen().root.query_tree().children:
                pid = w.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                if pid is not None and int(pid.value[0]) == process.pid and w.get_attributes().map_state == X.IsViewable:
                    return w

        def key(name):
            code = connection.keysym_to_keycode(XK.string_to_keysym(name))
            xtest.fake_input(connection, X.KeyPress, code)
            xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()
            time.sleep(0.2)

        def close_request(process):
            w = gui.wait_for(lambda: window(process), "mapped window")
            w.set_input_focus(X.RevertToParent, X.CurrentTime)
            w.send_event(protocol.event.ClientMessage(
                window=w, client_type=connection.intern_atom("WM_PROTOCOLS"),
                data=(32, [connection.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0]),
            ))
            connection.sync()
            time.sleep(0.5)

        h.state_path.parent.mkdir(parents=True)
        for index, source in enumerate([b'{"schema_version":', b'{"schema_version":4294967295}']):
            h.state_path.write_bytes(source)
            process, socket = h.window(f"degraded-{index}")
            workspace = h.rpc(socket, "workspace_create", name="Unsaved", root=str(h.root))["workspace_created"]["id"]
            close_request(process)
            assert process.poll() is None, "degraded window silently closed"
            key("Return")  # Cancel is the safe default.
            assert process.poll() is None
            assert any(w["id"] == workspace for w in h.tree(socket))
            close_request(process)
            assert process.poll() is None
            key("Tab")  # Move from Cancel to the explicit destructive response.
            key("Return")
            process.wait(timeout=10)
            assert process.returncode == 0
            assert h.state_path.read_bytes() == source, "startup failure must not overwrite the original"
            h.pass_check(f"degraded startup {index}: Cancel preserves work; explicit close preserves original file")

        h.state_path.unlink()
        process, socket = h.window("healthy")
        workspace = h.rpc(socket, "workspace_create", name="Saved", root=str(h.root))["workspace_created"]["id"]
        gui.wait_for(lambda: window(process), "healthy mapped window")
        h.close_window(process)
        process, socket = h.window("restored")
        gui.wait_for(lambda: window(process), "restored mapped window")
        assert any(w["id"] == workspace for w in h.tree(socket))
        h.close_window(process)
        h.pass_check("healthy startup saves and restores normally without extra confirmation")
        # Simulate same-boot PID reuse without changing kernel PID allocation.
        unrelated = h.spawn(["sleep", "120"])
        saved = json.loads(h.state_path.read_text())
        owner = next(w for w in saved["windows"] if workspace in w["workspace_order"])
        owner["owner_pid"] = unrelated.pid
        owner["owner_start_time"] = 0  # Deliberately not this live process's birth marker.
        h.state_path.write_text(json.dumps(saved))
        process, socket = h.window("pid-reused")
        gui.wait_for(lambda: window(process), "PID reuse restored window")
        assert any(w["id"] == workspace for w in h.tree(socket)), "reused PID hid saved workspace"
        assert unrelated.poll() is None, "recovery must not signal an unrelated process"
        second, second_socket = h.window("live-owner")
        gui.wait_for(lambda: window(second), "second mapped window")
        assert not any(w["id"] == workspace for w in h.tree(second_socket)), "live owner's workspace stolen"
        h.close_window(second)
        h.close_window(process)
        h.stop(unrelated)
        h.pass_check("PID reuse restores saved workspace; live owner and unrelated process are preserved")
        connection.close()
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
