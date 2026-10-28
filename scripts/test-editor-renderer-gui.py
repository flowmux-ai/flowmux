#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check renderer crash limits, native retry and clean close in an isolated GUI."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import signal
import sys
import time

from Xlib import X, XK, display
from Xlib.ext import xtest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "idle", Path(__file__).with_name("test-idle-stability-gui.py")
)
idle = importlib.util.module_from_spec(spec)
spec.loader.exec_module(idle)
gui = idle.gui


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
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell)}))
        work = h.root / "project"
        work.mkdir()
        document = work / "document.txt"
        document.write_text("Renderer fixture\n")
        h.env["GTK_A11Y"] = "atspi"
        h.env.pop("AT_SPI_BUS_ADDRESS", None)
        h.start_display()
        os.environ["DISPLAY"] = h.env["DISPLAY"]
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ.pop("AT_SPI_BUS_ADDRESS", None)
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        process, path = h.window("editor-renderer")
        workspace = h.rpc(path, "workspace_create", name="Renderer recovery", root=str(work))["workspace_created"]["id"]
        h.rpc(path, "workspace_focus", workspace=workspace)
        pane = h.workspace(path, workspace)["panes"][0]["id"]
        connection = display.Display(h.env["DISPLAY"])

        def focus():
            for window in connection.screen().root.query_tree().children:
                pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                if pid is not None and int(pid.value[0]) == process.pid and window.get_attributes().map_state == X.IsViewable:
                    window.set_input_focus(X.RevertToParent, X.CurrentTime)
                    connection.sync()
                    return True

        def key(name, *modifiers):
            codes = [connection.keysym_to_keycode(XK.string_to_keysym(k)) for k in (*modifiers, name)]
            assert all(codes)
            for code in codes:
                xtest.fake_input(connection, X.KeyPress, code)
            for code in reversed(codes):
                xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()
            time.sleep(.15)

        def find(name):
            desktop = Atspi.get_desktop(0)
            pending = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
            pending = [app for app in pending if app and app.get_process_id() == process.pid]
            while pending:
                node = pending.pop()
                if node:
                    if node.get_name() == name:
                        return node
                    pending.extend(node.get_child_at_index(i) for i in range(node.get_child_count()))

        def renderers():
            result = set()
            for pid in idle.descendants(process.pid):
                try:
                    if Path(f"/proc/{pid}/comm").read_text().startswith("WebKitWeb"):
                        result.add(pid)
                except FileNotFoundError:
                    pass
            return result

        def crash():
            owned = gui.wait_for(renderers, "owned editor renderer")
            assert len(owned) == 1, owned
            pid = next(iter(owned))
            descriptor = os.pidfd_open(pid)
            try:
                assert pid in idle.descendants(process.pid)
                signal.pidfd_send_signal(descriptor, signal.SIGKILL)
            finally:
                os.close(descriptor)
            gui.wait_for(lambda: pid not in renderers(), "renderer terminated")

        saved = 0

        def edit_and_save():
            nonlocal saved
            h.rpc(path, "surface_focus", pane=pane, surface=editor["id"])
            # The replacement process exists before Monaco is ready for native input.
            time.sleep(2)
            key("End", "Control_L")
            key("x")
            key("s", "Control_L")
            saved += 1
            gui.wait_for(lambda: document.read_text() == "Renderer fixture\n" + "x" * saved,
                         "recovered editor accepts input and saves")

        gui.wait_for(focus, "mapped editor window")
        time.sleep(.5)
        key("f", "Control_L", "Alt_L")
        key("Right", "Alt_L")
        key("Return")
        editor = gui.wait_for(lambda: next((t for w in h.tree(path) for p in w["panes"]
                                           for t in p["tabs"] if t["kind"] == "editor"), None), "editor tab")
        edit_and_save()
        for attempt in range(2):
            for _ in range(3):
                crash()
                gui.wait_for(renderers, "automatic replacement renderer")
                edit_and_save()
            crash()
            gui.wait_for(lambda: find("Editor stopped responding"), "native failure page")
            retry = gui.wait_for(lambda: find("Retry Editor"), "native retry button")
            assert not renderers(), "crash loop continued past the retry budget"
            if attempt == 0:
                assert retry.get_action_iface().do_action(0)
                gui.wait_for(renderers, "manual replacement renderer")
                edit_and_save()
                h.pass_check("native Retry restores document contents and editing after four renderer exits")
        h.close_window(process)
        h.pass_check("clean editor closes normally after exhausting the retry budget again")
        print("EDITOR_RENDERER_RECOVERY_OK", flush=True)
    finally:
        if connection:
            connection.close()
        h.cleanup()


if __name__ == "__main__":
    main()
