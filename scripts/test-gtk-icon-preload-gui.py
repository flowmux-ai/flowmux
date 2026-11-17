#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify the executable's GTK 4.14.5 mitigation in a private Xvfb and state tree."""
import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import sys

sys.dont_write_bytecode = True


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for name in ("gui", "cli"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    scripts = Path(__file__).resolve().parent
    spec = importlib.util.spec_from_file_location("gui", scripts / "test-ssh-workspace-gui.py")
    gui = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(gui)
    h = gui.Harness(args)
    try:
        probe = h.root / "icon-preload.so"
        flags = shlex.split(subprocess.check_output(
            ["pkg-config", "--cflags", "--libs", "gtk4"], text=True))
        subprocess.run(["cc", "-shared", "-fPIC", "-Wall", "-Wextra", "-Werror",
                        str(scripts / "test-gtk-icon-preload.c"), "-o", str(probe), *flags], check=True)
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell)}))
        h.start_display()
        h.env.update(LD_PRELOAD=str(probe), G_DEBUG="fatal-criticals")
        h.env.pop("LD_LIBRARY_PATH", None)
        process, sock = h.window("icon-preload")
        log = h.root / "icon-preload.log"

        def completed():
            assert process.poll() is None, f"GUI exited with {process.returncode}; see {log}"
            return "FLOWMUX_GTK_ICON_PRELOAD_OK" in log.read_text()

        gui.wait_for(completed, "10000 GTK preloads on the lookup thread", timeout=60)
        maps = Path(f"/proc/{process.pid}/maps").read_text()
        print("GTK_LOADED:", sorted({line.split()[-1] for line in maps.splitlines()
                                     if "libgtk-4.so" in line}), flush=True)
        workspace = h.rpc(sock, "workspace_create", name="Icon preload verified",
                          root=str(h.root))["workspace_created"]["id"]
        h.rpc(sock, "workspace_focus", workspace=workspace)
        pane = h.workspace(sock, workspace)["panes"][0]["id"]
        split = h.rpc(sock, "pane_split", pane=pane, direction="vertical")["pane_split_done"]["new_pane"]
        h.send(sock, split, "printf 'Icon preload verification complete\\n'")
        gui.wait_for(lambda: "Icon preload verification complete" in h.screen(sock, split),
                     "terminal response after icon stress")
        assert len(h.workspace(sock, workspace)["panes"]) == 2
        h.close_window(process)
        h.pass_check("10000 cold icon preloads, workspace/split/input and clean shutdown")
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
