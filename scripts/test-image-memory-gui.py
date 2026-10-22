# SPDX-License-Identifier: GPL-3.0-or-later
"""Check oversized raster rejection and normal image opening in an isolated GUI."""
import argparse
from contextlib import closing
import importlib.util
import json
from pathlib import Path
import shlex
import signal
import struct
import sys
import time
import zlib
from PIL import Image
from Xlib import X, XK, display, protocol
from Xlib.ext import xtest

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("gui", REPO / "scripts/test-ssh-workspace-gui.py")
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


def png(path, size):
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))
    compressor = zlib.compressobj(6)
    row = b"\0" + bytes([24, 180, 100]) * size
    with path.open("wb") as output:
        output.write(b"\x89PNG\r\n\x1a\n")
        output.write(chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 2, 0, 0, 0)))
        for _ in range(size):
            data = compressor.compress(row)
            if data:
                output.write(chunk(b"IDAT", data))
        output.write(chunk(b"IDAT", compressor.flush()))
        output.write(chunk(b"IEND", b""))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--gui", default=str(REPO / "target/debug/flowmux"))
    parser.add_argument("--cli", default=str(REPO / "target/debug/flowmuxctl"))
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    h = gui.Harness(args)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        image_path = h.root / "large.png"
        png(image_path, 10000)
        print(f"PNG_BYTES={image_path.stat().st_size} PIXELS={10000**2}", flush=True)
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell), "terminal_minimap_enabled": False}))
        h.start_display()
        process, socket = h.window("image-probe")
        ws = h.rpc(socket, "workspace_create", name="Image memory probe", root=str(h.root))["workspace_created"]["id"]
        h.rpc(socket, "workspace_focus", workspace=ws)
        pane = h.workspace(socket, ws)["panes"][0]["id"]
        h.rpc(socket, "pane_focus", pane=pane)
        with closing(display.Display(h.env["DISPLAY"])) as connection:
            def windows():
                result = []
                for window in connection.screen().root.query_tree().children:
                    pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                    if pid is not None and int(pid.value[0]) == process.pid and window.get_attributes().map_state == X.IsViewable:
                        result.append(window)
                return result
            main_window = gui.wait_for(lambda: windows(), "main window")[0]
            h.send(socket, pane, "printf '\\033[2J\\033[H\\033[48;2;255;0;255m          \\033[0m\\r\\n%s\\n' " + shlex.quote(str(image_path)))
            gui.wait_for(lambda: str(image_path) in h.screen(socket, pane), "image path output")
            def marker():
                root = connection.screen().root
                geo = root.get_geometry()
                raw = root.get_image(0, 0, geo.width, geo.height, X.ZPixmap, 0xffffffff)
                shot = Image.frombytes("RGB", (geo.width, geo.height), raw.data, "raw", "BGRX")
                points = [(i % shot.width, i // shot.width) for i, c in enumerate(shot.get_flattened_data()) if c == (255, 0, 255)]
                if points:
                    left, top = min(x for x, y in points), min(y for x, y in points)
                    width, height = max(x for x, y in points) - left + 1, max(y for x, y in points) - top + 1
                    assert width % 10 == 0
                    return left, top, width // 10, height
            x, y, cw, ch = gui.wait_for(marker, "terminal marker")
            def memory(label):
                text = (Path("/proc") / str(process.pid) / "status").read_text()
                print(label, {line.split(':')[0]: line.split(':')[1].strip() for line in text.splitlines() if line.startswith(("VmRSS:", "VmHWM:"))}, flush=True)
            memory("BEFORE")
            main_window.set_input_focus(X.RevertToParent, X.CurrentTime)
            xtest.fake_input(connection, X.MotionNotify, x=x+10*cw, y=y+ch+ch//2)
            connection.sync()
            time.sleep(.5)
            control = connection.keysym_to_keycode(XK.string_to_keysym("Control_L"))
            xtest.fake_input(connection, X.KeyPress, control)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            xtest.fake_input(connection, X.KeyRelease, control)
            connection.sync()
            log = h.root / "image-probe.log"
            gui.wait_for(lambda: "64 MiB decoding limit" in log.read_text(), "oversized image rejected")
            assert process.poll() is None, "oversized image terminated the GUI"
            memory("REJECTED")
            root = connection.screen().root
            geo = root.get_geometry()
            raw = root.get_image(0, 0, geo.width, geo.height, X.ZPixmap, 0xffffffff)
            Image.frombytes("RGB", (geo.width, geo.height), raw.data, "raw", "BGRX").save(h.root / "rejection.png")
            escape = connection.keysym_to_keycode(XK.string_to_keysym("Escape"))
            xtest.fake_input(connection, X.KeyPress, escape)
            xtest.fake_input(connection, X.KeyRelease, escape)
            connection.sync()
            time.sleep(.5)
            assert len(windows()) == 1, "rejection should not launch a viewer"
            # Replace the same file so the terminal link exercises a normal image too.
            png(image_path, 32)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            xtest.fake_input(connection, X.KeyPress, control)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            xtest.fake_input(connection, X.KeyRelease, control)
            connection.sync()
            viewer = gui.wait_for(lambda: next((w for w in windows() if w.id != main_window.id), None), "normal image opens")
            memory("OPENED")
            viewer.send_event(protocol.event.ClientMessage(window=viewer, client_type=connection.intern_atom("WM_PROTOCOLS"), data=(32, [connection.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0])))
            connection.sync()
            gui.wait_for(lambda: len(windows()) == 1, "image closed")
            time.sleep(3)
            memory("CLOSED")
        h.rpc(socket, "ping")
        h.close_window(process)
        h.pass_check("large image rejected, small image opens, GUI stays responsive")
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
