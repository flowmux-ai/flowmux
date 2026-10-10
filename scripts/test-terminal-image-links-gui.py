#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Ctrl-click parenthesized image paths in an isolated Flowmux GUI (Linux).

Requires Xvfb, python-xlib and Pillow.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import sys
import time
import unicodedata

from PIL import Image
from Xlib import X, XK, display, error, protocol
from Xlib.ext import xtest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "gui", Path(__file__).with_name("test-ssh-workspace-gui.py"))
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
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({
            "default_shell": str(shell), "terminal_minimap_enabled": False}))
        h.start_display()
        connection = display.Display(h.env["DISPLAY"])
        process, socket = h.window("image-links")
        created = h.rpc(socket, "workspace_create", name="Image links", root=str(h.root))
        workspace = h.workspace(socket, created["workspace_created"]["id"])
        pane = workspace["panes"][0]["id"]

        def windows():
            result = []
            for window in connection.screen().root.query_tree().children:
                try:
                    pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                    if (pid is not None and int(pid.value[0]) == process.pid
                            and window.get_attributes().map_state == X.IsViewable):
                        result.append(window)
                except error.BadWindow:
                    pass
            return result

        main_window = gui.wait_for(lambda: windows() and windows()[0], "main window")

        def screenshot():
            root = connection.screen().root
            geometry = root.get_geometry()
            raw = root.get_image(0, 0, geometry.width, geometry.height, X.ZPixmap, 0xffffffff)
            return Image.frombytes("RGB", (geometry.width, geometry.height), raw.data, "raw", "BGRX")

        def marker():
            shot = screenshot()
            pixels = [(i % shot.width, i // shot.width) for i, color in enumerate(shot.getdata())
                      if color == (255, 0, 255)]
            if pixels:
                left, top = min(x for x, _ in pixels), min(y for _, y in pixels)
                width = max(x for x, _ in pixels) - left + 1
                height = max(y for _, y in pixels) - top + 1
                assert width % 10 == 0, width
                return left, top, width // 10, height

        def click(prefix, geometry):
            left, top, cell_width, cell_height = geometry
            column = sum(2 if unicodedata.east_asian_width(c) in ("W", "F") else 1 for c in prefix)
            main_window.set_input_focus(X.RevertToParent, X.CurrentTime)
            control = connection.keysym_to_keycode(XK.string_to_keysym("Control_L"))
            xtest.fake_input(connection, X.MotionNotify, x=left + column * cell_width + cell_width // 2, y=top + cell_height + cell_height // 2)
            xtest.fake_input(connection, X.KeyPress, control)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            xtest.fake_input(connection, X.KeyRelease, control)
            connection.sync()

        for name, relative, closing in [
            ("agents-working.png", False, ")"),
            ("agents-working.png", False, ""),
            ("(draft(v2)).png", True, ")"),
        ]:
            image_path = h.root / name
            Image.new("RGB", (320, 200), (24, 180, 100)).save(image_path)
            path_text = name if relative else str(image_path)
            line = f"검증 화면 ({path_text}{closing}"
            # Ten colored cells measure the actual terminal grid, including font scaling.
            h.send(socket, pane, "printf '\\033[2J\\033[H\\033[48;2;255;0;255m          \\033[0m\\r\\n%s\\n' " + shlex.quote(line))
            gui.wait_for(lambda: line in h.screen(socket, pane), "image path output")
            geometry = gui.wait_for(marker, "terminal cell marker")
            click("검증 화면 ", geometry)
            time.sleep(.3)
            assert len(windows()) == 1, "surrounding parenthesis opened a viewer"
            click("검증 화면 (" + path_text[:-2], geometry)
            viewer = gui.wait_for(lambda: next((w for w in windows() if w.id != main_window.id), None),
                                  "image viewer", timeout=8)
            title = viewer.get_full_property(connection.intern_atom("_NET_WM_NAME"), connection.intern_atom("UTF8_STRING"))
            assert title.value.decode() == name, title.value

            def loaded_pixels():
                geometry = viewer.get_geometry()
                raw = viewer.get_image(geometry.width // 2, geometry.height // 2, 1, 1, X.ZPixmap, 0xffffffff)
                return Image.frombytes("RGB", (1, 1), raw.data, "raw", "BGRX").getpixel((0, 0)) == (24, 180, 100)

            gui.wait_for(loaded_pixels, "decoded PNG visible in viewer")
            screenshot().save(h.root / f"image-link-{relative}-{bool(closing)}.png")
            h.pass_check(f"{line}: wrapper excluded, Ctrl-click displays PNG")
            viewer.send_event(protocol.event.ClientMessage(
                window=viewer, client_type=connection.intern_atom("WM_PROTOCOLS"),
                data=(32, [connection.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0])))
            connection.sync()
            gui.wait_for(lambda: len(windows()) == 1, "viewer closed")
        h.close_window(process)
    finally:
        if connection:
            connection.close()
        h.cleanup()


if __name__ == "__main__":
    main()
