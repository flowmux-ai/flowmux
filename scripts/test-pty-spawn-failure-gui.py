#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise real PTY allocation failures in an isolated Linux GUI process."""
import argparse
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
    try:
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell)}))
        h.start_display()
        process, path = h.window("pty-failure")
        workspace = h.rpc(path, "workspace_create", name="PTY failure", root=str(h.root))["workspace_created"]["id"]
        h.rpc(path, "workspace_focus", workspace=workspace)
        pane = h.workspace(path, workspace)["panes"][0]["id"]
        pid_file = h.root / "original-shell.pid"
        h.send(path, pane, f"echo $$ > {pid_file}")
        gui.wait_for(pid_file.exists, "original terminal shell")
        original_pid = int(pid_file.read_text())
        time.sleep(0.5)  # Let the initial window mapping and shell callbacks settle.

        def fail(verb, **fields):
            before = h.tree(path)
            # Admit the IPC connection first, then exhaust only this GUI's FDs.
            # The existing socket must reach PTY creation, not fail at accept().
            with socket.socket(socket.AF_UNIX) as stream:
                stream.settimeout(4)
                stream.connect(str(path))
                reader = stream.makefile("rb")
                stream.sendall(b'{"id":1,"kind":"request","verb":"ping"}\n')
                assert "pong" in json.loads(reader.readline())
                limits = resource.prlimit(process.pid, resource.RLIMIT_NOFILE)
                # VTE releases spawn-time FDs asynchronously; wait before
                # choosing a cap so those closes cannot open a hole below it.
                def fd_table():
                    return {int(fd.name) for fd in Path(f"/proc/{process.pid}/fd").iterdir()}

                open_fds = fd_table()
                deadline = time.monotonic() + 3
                while time.monotonic() < deadline:
                    time.sleep(0.2)
                    current = fd_table()
                    if current == open_fds:
                        break
                    open_fds = current
                else:
                    raise AssertionError("GUI file descriptor table did not settle")
                limit = next(fd for fd in range(max(open_fds) + 2) if fd not in open_fds)
                try:
                    resource.prlimit(process.pid, resource.RLIMIT_NOFILE, (limit, limits[1]))
                    start = time.monotonic()
                    stream.sendall((json.dumps({"id":2,"kind":"request","verb":verb, **fields}) + "\n").encode())
                    response = json.loads(reader.readline())
                    assert "error" in response, response
                    assert "terminal" in response["error"]["message"].lower(), response
                    assert time.monotonic() - start < 4
                finally:
                    resource.prlimit(process.pid, resource.RLIMIT_NOFILE, limits)
            assert process.poll() is None
            assert h.tree(path) == before, "failed operation changed the pane/tab model"
            assert Path(f"/proc/{original_pid}").exists(), "existing shell was terminated"
            h.send(path, pane, "printf 'still-responsive\\n'")
            gui.wait_for(lambda: "still-responsive" in h.screen(path, pane), "GTK dispatch after allocation failure")
            h.pass_check(f"{verb}: OS failure returned; model, original shell, and GTK dispatch preserved")

        fail("pane_split", pane=pane, direction="vertical")
        sibling = h.rpc(path, "pane_split", pane=pane, direction="vertical")["pane_split_done"]["new_pane"]
        fail("pane_split", pane=sibling, direction="horizontal")
        # Multiple tabs, with the first active, catches rollback choosing the
        # last tab rather than the user's previous active tab.
        h.rpc(path, "pane_focus", pane=pane)
        first = h.workspace(path, workspace)["panes"][0]["tabs"][0]["id"]
        h.rpc(path, "surface_create", workspace=workspace, cwd=None)
        h.rpc(path, "surface_focus", pane=pane, surface=first)
        fail("surface_create", workspace=workspace, cwd=None)
        h.rpc(path, "surface_create", workspace=workspace, cwd=None)
        h.rpc(path, "pane_split", pane=sibling, direction="horizontal")
        h.pass_check("new tabs and nested splits succeed after restoring resource limits")
        assert "panicked at" not in (h.root / "pty-failure.log").read_text()
        h.close_window(process)
    finally:
        h.cleanup()


if __name__ == "__main__":
    main()
