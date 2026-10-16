#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise validated file links and Markdown click modifiers in an isolated Flowmux GUI (Linux).

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

from PIL import Image, ImageChops
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
        terminal = workspace["panes"][0]["tabs"][0]["id"]
        workspace_id = workspace["id"]

        def windows(include_markdown=False):
            result = []
            for window in connection.screen().root.query_tree().children:
                try:
                    pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                    owned = pid is not None and int(pid.value[0]) == process.pid
                    if include_markdown and pid is not None and not owned:
                        try:
                            status = Path(f"/proc/{int(pid.value[0])}/status").read_text()
                            owned = f"PPid:\t{process.pid}\n" in status
                        except FileNotFoundError:
                            pass
                    if (owned
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

        def click(prefix, geometry, shift=False):
            left, top, cell_width, cell_height = geometry
            column = sum(2 if unicodedata.east_asian_width(c) in ("W", "F") else 1 for c in prefix)
            main_window.set_input_focus(X.RevertToParent, X.CurrentTime)
            control = connection.keysym_to_keycode(XK.string_to_keysym("Control_L"))
            shift_key = connection.keysym_to_keycode(XK.string_to_keysym("Shift_L"))
            xtest.fake_input(connection, X.MotionNotify, x=left + column * cell_width + cell_width // 2, y=top + cell_height + cell_height // 2)
            connection.sync()
            time.sleep(.25)  # Wait for the asynchronous path validation.
            xtest.fake_input(connection, X.KeyPress, control)
            if shift:
                xtest.fake_input(connection, X.KeyPress, shift_key)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            if shift:
                xtest.fake_input(connection, X.KeyRelease, shift_key)
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
        def editor_tabs():
            return [tab for p in h.workspace(socket, workspace_id)["panes"]
                    for tab in p["tabs"] if tab["kind"] == "editor"]

        def show_path(path_text):
            h.rpc(socket, "surface_focus", pane=pane, surface=terminal)
            xtest.fake_input(connection, X.MotionNotify, x=1, y=1)
            connection.sync()
            h.send(socket, pane, "printf '\\033[2J\\033[H\\033[48;2;255;0;255m          \\033[0m\\r\\n%s\\n' " + shlex.quote(path_text))
            gui.wait_for(lambda: path_text in h.screen(socket, pane), "source path output")
            return gui.wait_for(marker, "terminal cell marker")

        def assert_hover(path_text, geometry, expected, prefix=""):
            left, top, cw, ch = geometry
            left += sum(2 if unicodedata.east_asian_width(c) in ("W", "F") else 1
                        for c in prefix) * cw
            box = (left, top + ch, left + len(path_text) * cw, top + 2 * ch)
            time.sleep(.15)
            before = screenshot().crop(box)
            xtest.fake_input(connection, X.MotionNotify, x=left + cw // 2, y=top + ch + ch // 2)
            connection.sync()
            time.sleep(1)
            after = screenshot().crop(box)
            difference = ImageChops.difference(before, after)
            changed = max(sum(difference.getpixel((x, y)) != (0, 0, 0)
                              for x in range(difference.width)) for y in range(ch))
            screenshot().save(h.root / ("hover-valid.png" if expected else "hover-invalid.png"))
            assert (changed > difference.width * .7) == expected, (path_text, changed, difference.width)

        for name, absolute in [("main.cc", False), ("main.c", False), ("main.cpp", True),
                               ("main.dart", False), ("index.html", False), ("app.js", False),
                               ("settings.json", False), ("notes.txt", False), ("Dockerfile", False),
                               ("readme.md", False), ("guide.MARKDOWN", True)]:
            file_path = h.root / name
            file_path.write_text("// terminal editor link fixture\n")
            path_text = str(file_path) if absolute else name
            geometry = show_path(path_text)
            assert_hover(path_text, geometry, True)
            previous = len(editor_tabs())
            click(path_text[:len(path_text) // 2], geometry)
            tabs = gui.wait_for(lambda: editor_tabs() if len(editor_tabs()) == previous + 1 else None,
                                "Ctrl-click adds editor tab")
            gui.wait_for(lambda: editor_tabs()[-1]["title"] == name, "editor file title")
            if name == "settings.json":
                time.sleep(3)
                screenshot().save(h.root / "json-editor.png")
                for key, modifiers in [("End", ["Control_L"]), ("x", []), ("s", ["Control_L"])]:
                    codes = [connection.keysym_to_keycode(XK.string_to_keysym(k)) for k in modifiers + [key]]
                    for code in codes:
                        xtest.fake_input(connection, X.KeyPress, code)
                    for code in reversed(codes):
                        xtest.fake_input(connection, X.KeyRelease, code)
                    connection.sync()
                    time.sleep(.15)
                gui.wait_for(lambda: file_path.read_text().endswith("x"), "clicked document is loaded and editable")
            assert len(windows(include_markdown=True)) == 1, "source file opened a viewer"
            h.pass_check(f"{path_text}: hover underline, Ctrl-click adds editor tab")

            if file_path.suffix.lower() in (".md", ".markdown"):
                file_path.write_text("# Mermaid\n\n```mermaid\nflowchart LR\nA[시작] --> B[완료]\nstyle A fill:#12ab34\n```\n")
                geometry = show_path(path_text)
                previous = len(editor_tabs())
                click(path_text[:len(path_text) // 2], geometry, shift=True)
                viewer = gui.wait_for(lambda: next((w for w in windows(include_markdown=True)
                                                   if w.id != main_window.id), None),
                                      "Ctrl+Shift-click opens Markdown viewer")
                title = viewer.get_full_property(connection.intern_atom("_NET_WM_NAME"),
                                                 connection.intern_atom("UTF8_STRING"))
                assert title.value.decode() == name, title.value
                assert len(editor_tabs()) == previous, "Markdown preview added an editor tab"

                def diagram_pixels():
                    size = viewer.get_geometry()
                    raw = viewer.get_image(0, 0, size.width, size.height, X.ZPixmap, 0xffffffff)
                    image = Image.frombytes("RGB", (size.width, size.height), raw.data, "raw", "BGRX")
                    return sum(r < 40 and 150 < g < 190 and b < 80 for r, g, b in image.getdata())

                gui.wait_for(lambda: diagram_pixels() > 500, "Mermaid diagram visible in Markdown viewer")
                before_zoom = diagram_pixels()
                viewer.set_input_focus(X.RevertToParent, X.CurrentTime)
                codes = [connection.keysym_to_keycode(XK.string_to_keysym(k)) for k in ["Control_L", "equal"]]
                for code in codes:
                    xtest.fake_input(connection, X.KeyPress, code)
                for code in reversed(codes):
                    xtest.fake_input(connection, X.KeyRelease, code)
                connection.sync()
                gui.wait_for(lambda: diagram_pixels() > before_zoom * 1.2, "Mermaid diagram zooms in place")
                screenshot().save(h.root / f"markdown-viewer-{name}.png")
                viewer.send_event(protocol.event.ClientMessage(
                    window=viewer, client_type=connection.intern_atom("WM_PROTOCOLS"),
                    data=(32, [connection.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0])))
                connection.sync()
                gui.wait_for(lambda: len(windows(include_markdown=True)) == 1, "Markdown viewer closed")
                h.pass_check(f"{path_text}: Ctrl+Shift-click displays Mermaid without adding a tab")

        for name, absolute in [("artifacts/tizen-modules/README.md", False),
                               ("artifacts/tizen-modules/README.md", True),
                               ("(draft(v2)).md", False)]:
            file_path = h.root / name
            file_path.parent.mkdir(parents=True, exist_ok=True)
            file_path.write_text("# Wrapped Markdown link\n")
            path_text = str(file_path) if absolute else name
            prefix = "전체 API 파일 링크와 품질 검토 결과 ("
            line = f"{prefix}{path_text})를 정리했습니다."
            geometry = show_path(line)
            assert_hover(path_text, geometry, True, prefix=prefix)
            previous = len(editor_tabs())
            click(prefix[:-1], geometry)
            time.sleep(.3)
            assert len(editor_tabs()) == previous, "wrapper opened an editor"
            click(prefix + path_text + ")", geometry)
            time.sleep(.3)
            assert len(editor_tabs()) == previous, "prose suffix opened an editor"
            click(prefix + path_text[:len(path_text) // 2], geometry)
            gui.wait_for(lambda: len(editor_tabs()) == previous + 1,
                         "wrapped Markdown path adds editor tab")
            gui.wait_for(lambda: editor_tabs()[-1]["title"] == file_path.name,
                         "wrapped Markdown file title")
            h.pass_check(f"{line}: only the path is underlined and clickable")

        for kind in ["binary", "directory", "unreadable", "large", "broken-symlink"]:
            inner = f"unopenable-{kind}.md"
            (h.root / inner).write_text("# Different document\n")
            name = f"({inner})를"
            path = h.root / name
            if kind == "binary":
                path.write_bytes(b"binary\x00data")
            elif kind == "directory":
                path.mkdir()
            elif kind == "unreadable":
                path.write_text("# Private document\n")
                path.chmod(0)
                try:
                    with path.open("rb"):
                        print("SKIP: process can bypass file permissions", flush=True)
                        continue
                except PermissionError:
                    pass
            elif kind == "large":
                with path.open("wb") as fixture:
                    fixture.truncate(16 * 1024 * 1024 + 1)
            else:
                path.symlink_to("absent-target")
            geometry = show_path(name)
            assert_hover(inner, geometry, False, prefix="(")
            previous = len(editor_tabs())
            for shift in [False, True]:
                click("(" + inner[:len(inner) // 2], geometry, shift=shift)
                time.sleep(.3)
                assert len(editor_tabs()) == previous, f"{kind} opened a different file"
                assert len(windows(include_markdown=True)) == 1, f"{kind} opened a viewer"
            h.pass_check(f"{name}: existing {kind} never opens the inner document")

        for name, content in [("missing.cc", None), ("missing.md", None),
                              ("(readme.md).missing", None), ("binary.txt", b"binary\x00data")]:
            if content is not None:
                (h.root / name).write_bytes(content)
            geometry = show_path(name)
            assert_hover(name, geometry, False)
            previous = len(editor_tabs())
            click(name[:len(name) // 2], geometry)
            time.sleep(.3)
            assert len(editor_tabs()) == previous, "invalid file opened editor"
            if name.endswith(".md"):
                click(name[:len(name) // 2], geometry, shift=True)
                time.sleep(.3)
                assert len(windows(include_markdown=True)) == 1, "missing Markdown opened a viewer"
            h.pass_check(f"{name}: no underline or editor tab")
        h.close_window(process)
    finally:
        if connection:
            connection.close()
        h.cleanup()


if __name__ == "__main__":
    main()
