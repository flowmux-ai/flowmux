#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exhaust only an isolated GUI's FDs and verify both IPC listeners recover."""
import argparse
from contextlib import ExitStack
import importlib.util
import json
from pathlib import Path
import resource
import socket
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
    args.gui = str(Path(args.gui).resolve(strict=True))
    args.cli = str(Path(args.cli).resolve(strict=True))
    h = gui.Harness(args)
    h.env["NO_COLOR"] = "1"
    try:
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell)}))
        h.start_display()
        process, path = h.window("ipc-recovery")
        workspace = h.rpc(path, "workspace_create", name="Recovery", root=str(h.root))["workspace_created"]["id"]
        pane = h.workspace(path, workspace)["panes"][0]["id"]
        h.rpc(path, "workspace_focus", workspace=workspace)
        h.send(path, pane, "printf 'ready-for-ipc-%s\\n' test")
        gui.wait_for(lambda: "ready-for-ipc-test" in h.screen(path, pane), "terminal ready")
        time.sleep(1)
        before = h.tree(path)
        log = h.root / "ipc-recovery.log"
        for endpoint in (path, Path(str(path) + ".ctl")):
            control = "true" if endpoint != path else "false"
            with socket.socket(socket.AF_UNIX) as admitted, ExitStack() as connections:
                admitted.settimeout(3)
                admitted.connect(str(path))
                reader = connections.enter_context(admitted.makefile("rb"))
                ping = b'{"id":1,"kind":"request","verb":"ping"}\n'
                admitted.sendall(ping)
                assert "pong" in json.loads(reader.readline())
                time.sleep(.5)
                # Keep poll() valid; fill the spare slots with admitted connections.
                limits = resource.prlimit(process.pid, resource.RLIMIT_NOFILE)
                offset = log.stat().st_size
                started = time.monotonic()
                try:
                    limit = max(int(fd.name) for fd in Path(f"/proc/{process.pid}/fd").iterdir()) + 8
                    resource.prlimit(process.pid, resource.RLIMIT_NOFILE, (limit, limits[1]))
                    # Fill regular admission so queued probes cannot hit the control pool's cap.
                    for _ in range(32):
                        pending = connections.enter_context(socket.socket(socket.AF_UNIX))
                        pending.settimeout(3)
                        pending.connect(str(path))
                        pending.sendall(ping)
                    pending = connections.enter_context(socket.socket(socket.AF_UNIX))
                    pending.settimeout(3)
                    pending.connect(str(endpoint))
                    pending.sendall(ping)
                    gui.wait_for(lambda: any(
                        "IPC accept failed; retrying" in line and f"control={control}" in line
                        for line in log.read_text()[offset:].splitlines()), "target listener accept failure", timeout=3)
                    time.sleep(.4)
                    admitted.sendall(ping)
                    assert "pong" in json.loads(reader.readline()), "admitted requests stopped"
                finally:
                    resource.prlimit(process.pid, resource.RLIMIT_NOFILE, limits)
                with pending.makefile("rb") as response:
                    assert "pong" in json.loads(response.readline()), "listener did not recover"
                errors = log.read_text()[offset:].count("IPC accept failed; retrying")
                assert 1 <= errors <= (time.monotonic() - started) * 10 + 2, f"accept retries missing or spinning: {errors}"
            assert process.poll() is None
            assert h.tree(path) == before, "FD exhaustion changed the workspace"
            assert "pong" in h.rpc(Path(str(path) + ".ctl"), "ping")
            h.pass_check(f"{endpoint.name}: queued and admitted requests survive FD exhaustion")
        h.send(path, pane, "printf 'ipc-recovery-%s\\n' terminal-ok")
        gui.wait_for(lambda: "ipc-recovery-terminal-ok" in h.screen(path, pane), "terminal after recovery")
        h.rpc(path, "pane_split", pane=pane, direction="vertical")
        h.pass_check("terminal input and pane creation work after both listeners recover")
        assert "ipc server exited" not in log.read_text()
        h.close_window(process)
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
