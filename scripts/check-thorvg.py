#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check the macOS viewer's runtime C API and SVG/Lottie/PNG loaders."""
import base64
import ctypes as c
from pathlib import Path
import sys

SYMBOLS = """tvg_engine_init tvg_engine_term tvg_swcanvas_create tvg_swcanvas_set_target
tvg_canvas_destroy tvg_canvas_add tvg_canvas_update tvg_canvas_draw tvg_canvas_sync
tvg_picture_new tvg_picture_load tvg_picture_load_data tvg_picture_load_raw
tvg_picture_set_size tvg_picture_get_size tvg_paint_ref tvg_paint_unref tvg_paint_rel
tvg_animation_new tvg_animation_del tvg_animation_set_frame tvg_animation_get_picture
tvg_animation_get_total_frame tvg_animation_get_duration
""".split()


def check(path):
    try:
        lib = c.CDLL(str(path))
        for name in SYMBOLS:
            getattr(lib, name)
    except (OSError, AttributeError):
        return False
    lib.tvg_engine_init.argtypes = [c.c_uint]
    lib.tvg_picture_new.restype = c.c_void_p
    lib.tvg_picture_load_data.argtypes = [c.c_void_p, c.c_char_p, c.c_uint,
                                         c.c_char_p, c.c_char_p, c.c_bool]
    lib.tvg_paint_rel.argtypes = [c.c_void_p]
    if lib.tvg_engine_init(0) != 0:
        return False
    try:
        for kind, data in [
            (b"svg", b'<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="red"/></svg>'),
            (b"lottie", b'{"v":"5.7.4","fr":30,"ip":0,"op":30,"w":2,"h":2,"layers":[]}'),
            (b"png", base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==")),
        ]:
            picture = lib.tvg_picture_new()
            if not picture:
                return False
            try:
                if lib.tvg_picture_load_data(picture, data, len(data), kind, b"", True) != 0:
                    return False
            finally:
                lib.tvg_paint_rel(picture)
        return True
    finally:
        lib.tvg_engine_term()


def candidates():
    yield Path.home() / ".local/lib/libthorvg-1.dylib"
    for prefix in ("/opt/homebrew/opt/thorvg", "/usr/local/opt/thorvg",
                   "/opt/homebrew", "/usr/local"):
        yield Path(prefix) / "lib/libthorvg-1.dylib"
    yield "libthorvg-1.dylib"
    yield "libthorvg.dylib"


if __name__ == "__main__":
    sys.exit(0 if any(check(path) for path in candidates()) else 1)
