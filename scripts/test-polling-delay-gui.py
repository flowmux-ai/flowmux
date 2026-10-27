#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Prove stalled /proc cwd reads do not accumulate GUI background workers."""

import argparse
import importlib.util
import json
from pathlib import Path
import signal
import subprocess
import sys
import time

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
    for name in ("gui", "cli"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = gui.Harness(args)
    flag, entered = h.root / "delay", h.root / "entered"
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell)}))
        library = h.root / "delay.so"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC", "-o", str(library),
                        str(Path(__file__).with_name("test-polling-delay.c")), "-ldl"], check=True)
        h.start_display()
        h.env.update(LD_PRELOAD=str(library), FLOWMUX_TEST_POLL_FLAG=str(flag),
                     FLOWMUX_TEST_POLL_ENTERED=str(entered))
        process, socket = h.window("polling-delay")
        workspace = h.rpc(socket, "workspace_create", name="Delayed cwd", root=str(h.root))["workspace_created"]["id"]
        h.rpc(socket, "workspace_focus", workspace=workspace)
        time.sleep(3)
        pane = h.workspace(socket, workspace)["panes"][0]["id"]
        # Clear the proxy's initial OSC 7 to exercise the non-announcing shell fallback.
        h.send(socket, pane, "printf '\\033]7;\\007'")
        time.sleep(.3)
        flag.write_text(str(process.pid))
        gui.wait_for(lambda: entered.exists(), "cwd poll reached injected stall")
        for _ in range(8):
            assert process.poll() is None, "GUI exited during stalled cwd poll"
            start = time.monotonic()
            h.tree(socket)
            assert time.monotonic() - start < 2, "stalled poll blocked the GUI"
            time.sleep(1)
        calls = entered.read_text().splitlines()
        flag.unlink()
        print(f"STALLED_CWD_CALLS={len(calls)}", flush=True)
        assert len(calls) == 1, f"periodic poll accumulated {len(calls)} blocked workers: {calls}"
        time.sleep(2)
        h.send(socket, pane, "printf 'polling-%s\\n' resumed")
        gui.wait_for(lambda: "polling-resumed" in h.screen(socket, pane), "terminal responds after release")
        h.close_window(process)
        h.pass_check("one stalled cwd worker, responsive GUI, terminal resumes and window closes normally")
    finally:
        flag.unlink(missing_ok=True)
        h.cleanup()


if __name__ == "__main__":
    main()
