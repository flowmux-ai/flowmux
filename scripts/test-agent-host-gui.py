#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify same-process Host reconnection through an isolated Flowmux/VTE view.

The fixture launches the Host and agent explicitly. GUI management, opening and
saved tab reconnection use the application's own paths, without native resume.
"""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import re
import signal
import socket
import subprocess
import sys
import time
from types import SimpleNamespace
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("gui_fixture", ROOT / "scripts/test-ssh-workspace-gui.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class HostClient:
    def __init__(self, path, role):
        self.stream = socket.socket(socket.AF_UNIX)
        self.stream.settimeout(10)
        self.stream.connect(str(path))
        self.reader = self.stream.makefile("rb")
        self.identifier = 0
        self.call("hello", version=1, role=role)

    def call(self, op, **fields):
        self.identifier += 1
        request = {"id": self.identifier, "kind": "request", "verb": "agent_host",
                   "request": {"op": op, **fields}}
        self.stream.sendall((json.dumps(request) + "\n").encode())
        response = json.loads(self.reader.readline())
        assert response["id"] == self.identifier, response
        assert "error" not in response, response
        return response["agent_host"]["response"]

    def close(self):
        self.reader.close()
        self.stream.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default=str(ROOT / "target/debug/flowmux"))
    parser.add_argument("--cli", default=str(ROOT / "target/debug/flowmuxctl"))
    parser.add_argument("--host", default=str(ROOT / "target/debug/flowmux-daemon"))
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for name in ("gui", "cli", "host"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    harness = fixture.Harness(SimpleNamespace(gui=args.gui, cli=args.cli, protected_pid=args.protected_pid))
    options = harness.root / "config/flowmux/options.json"
    options.parent.mkdir(parents=True, exist_ok=True)
    options.write_text(json.dumps({"default_shell": "/bin/sh", "auto_resume_agent_sessions": True}))
    host = None
    control_connection = None
    agent_pid = None
    birth = None
    try:
        harness.start_display()
        app, gui_socket = harness.window("agent-host-first")
        host_socket = harness.root / "run/flowmux-agent-host.sock"
        with (harness.root / "host.log").open("w") as log:
            host = harness.spawn([args.host, "--agent-host", "--socket", str(host_socket)],
                                 stdout=log, stderr=log, cwd=harness.root)
        fixture.wait_for(host_socket.exists, "Agent Host socket")
        control_connection = HostClient(host_socket, "relay")
        provider = harness.root / "fixture-agent.py"
        provider.write_text('''import os, select, sys, time, tty
from pathlib import Path
root = Path(sys.argv[1])
tty.setraw(0)
with (root / 'starts').open('a') as file: file.write(str(os.getpid()) + '\\n')
counter = 0
received = 0
pending = b''
print('\\x1b[?1049h\\x1b[2J\\x1b[HSESSION:' + str(os.getpid()) + '\\r\\n한글 화면 e\\u0301', end='', flush=True)
while True:
    counter += 1
    (root / 'progress.tmp').write_text(str(counter))
    (root / 'progress.tmp').replace(root / 'progress')
    print('\\x1b[3;1HCOUNTER:' + str(counter) + '\\x1b[K', end='', flush=True)
    if (root / 'hold-input').exists():
        time.sleep(0.1)
        continue
    if select.select([0], [], [], 0.1)[0]:
        data = os.read(0, 16384)
        if not data: break
        received += len(data)
        (root / 'received-bytes').write_text(str(received))
        pending += data.replace(b'\\r', b'\\n')
        while b'\\n' in pending:
            line, pending = pending.split(b'\\n', 1)
            (root / 'last-line-size').write_text(str(len(line)))
            text = line.decode('utf-8')
            if text == 'quit': sys.exit(0)
            if len(text) > 1024: text = 'bulk'
            print('\\x1b[4;1HACK:<' + text + '>\\x1b[K', end='', flush=True)
''')
        launch = {"request_id": str(uuid.uuid4()), "provider": "fixture", "label": "isolated Agent Host fixture",
                  "argv": ["/usr/bin/python3", "-u", str(provider), str(harness.root)],
                  "cwd": str(harness.root), "env": list(harness.env.items()) + [["TERM", "xterm-256color"]],
                  "size": {"rows": 24, "cols": 80}}
        agent = control_connection.call("launch", launch=launch)["agent"]
        agent_pid = agent["pid"]
        assert agent["state"] == "running", agent
        birth = Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19]

        def pane(path):
            if not harness.tree(path):
                harness.rpc(path, "workspace_create", root=str(harness.root),
                            name="Agent Host fixture", background=False, team=False)
            fixture.wait_for(lambda: harness.tree(path) and harness.tree(path)[0]["panes"],
                             "fixture terminal pane")
            workspace = harness.tree(path)[0]
            target = workspace["panes"][0]["id"]
            fixture.wait_for(lambda: isinstance(harness.screen(path, target), str),
                             "restored terminal widget ready")
            return target

        target = pane(gui_socket)
        workspace = harness.tree(gui_socket)[0]["id"]
        shell_surface = harness.tree(gui_socket)[0]["panes"][0]["tabs"][0]["id"]
        harness.send(gui_socket, target, "printf 'GUI-SHELL:%s\\n' \"$$\"")
        fixture.wait_for(lambda: re.search(r"GUI-SHELL:(\d+)", harness.screen(gui_socket, target)),
                         "ordinary shell PID")
        shell_pid = int(re.search(r"GUI-SHELL:(\d+)", harness.screen(gui_socket, target)).group(1))
        shell_birth = Path(f"/proc/{shell_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19]
        command = [args.cli, "--socket", str(gui_socket), "--json", "agent", "open", agent["instance"],
                   "--pane", target]
        opened = subprocess.run(command, env=harness.env, check=True, capture_output=True, text=True)
        agent_surface = json.loads(opened.stdout)["surface_created"]["id"]
        duplicate = harness.rpc(gui_socket, "agent_view_open", pane=target, instance=agent["instance"])
        assert duplicate["surface_created"]["id"] == agent_surface, duplicate
        assert len(harness.tree(gui_socket)[0]["panes"][0]["tabs"]) == 2

        def verify_view(path):
            restored = next(ws for ws in harness.tree(path) if ws["id"] == workspace)
            restored_pane = next(item for item in restored["panes"] if item["id"] == target)
            assert any(tab["id"] == agent_surface for tab in restored_pane["tabs"]), restored_pane
            fixture.wait_for(lambda: f"SESSION:{agent_pid}" in harness.screen(path, target), "same-process screen")
            fixture.wait_for(lambda: "한글 화면" in harness.screen(path, target), "Hangul screen through live VTE")
            token = "한글 입력 " + uuid.uuid4().hex[:6]
            harness.send(path, target, token)
            fixture.wait_for(lambda: "ACK:<" + token + ">" in harness.screen(path, target), "fresh Hangul input acknowledgement")

        verify_view(gui_socket)
        harness.pass_check("real Flowmux/VTE shows Host-owned alternate screen and round-trips Hangul input")
        harness.rpc(gui_socket, "surface_close", pane=target, surface=agent_surface)
        reopened = harness.rpc(gui_socket, "agent_view_open", pane=target, instance=agent["instance"])
        agent_surface = reopened["surface_created"]["id"]
        verify_view(gui_socket)
        source = target
        target = harness.rpc(gui_socket, "pane_split", pane=source, direction="vertical")["pane_split_done"]["new_pane"]
        moved = harness.rpc(gui_socket, "agent_view_open", pane=target, instance=agent["instance"])
        assert moved["surface_created"]["id"] == agent_surface
        verify_view(gui_socket)
        relays = []
        for process in Path("/proc").iterdir():
            if not process.name.isdigit():
                continue
            try:
                fields = (process / "stat").read_text().rsplit(") ", 1)[1].split()
                argv = (process / "cmdline").read_bytes().split(b"\0")
            except (FileNotFoundError, PermissionError, ProcessLookupError):
                continue
            if int(fields[1]) == app.pid and b"agent-relay" in argv and agent["instance"].encode() in argv:
                relays.append(int(process.name))
        assert len(relays) == 1, "expected one relay owned by the isolated GUI"
        assert relays[0] not in harness.protected_pids and relays[0] != agent_pid
        os.kill(relays[0], signal.SIGTERM)
        fixture.wait_for(lambda: not fixture.alive(relays[0]), "isolated relay exits without stopping Agent")
        target = source
        moved = harness.rpc(gui_socket, "agent_view_open", pane=target, instance=agent["instance"])
        assert moved["surface_created"]["id"] == agent_surface
        verify_view(gui_socket)
        assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
        harness.pass_check("closing/reopening and moving an Agent tab with a disconnected relay reconnect the original execution")
        hold = harness.root / "hold-input"
        hold.touch()
        before = int((harness.root / "progress").read_text())
        harness.send(gui_socket, target, "x" * (512 * 1024))

        def rendered_counter():
            match = re.search(r"COUNTER:(\d+)", harness.screen(gui_socket, target))
            return match is not None and int(match.group(1)) >= before + 10

        fixture.wait_for(rendered_counter, "view keeps rendering while Host input is full")
        hold.unlink()
        fixture.wait_for(lambda: "ACK:<bulk>" in harness.screen(gui_socket, target),
                         "all queued input reaches the original agent")
        harness.pass_check("input backpressure keeps current screen updates flowing and preserves queued input")
        before = int((harness.root / "progress").read_text())
        harness.close_window(app)
        fixture.wait_for(lambda: int((harness.root / "progress").read_text()) >= before + 5, "work continues with GUI closed")
        assert host.poll() is None
        assert Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
        shell_stat = Path(f"/proc/{shell_pid}/stat")
        fixture.wait_for(lambda: not shell_stat.exists() or shell_stat.read_text().rsplit(") ", 1)[1].split()[19] != shell_birth,
                         "ordinary GUI-owned shell exits")
        harness.pass_check("closing the isolated GUI leaves the same agent/PTY running and progressing")
        harness.pass_check("ordinary terminal exits with GUI while managed Agent continues")
        app, gui_socket = harness.window("agent-host-reopened")
        instances = control_connection.call("list")["agents"]
        assert len(instances) == 1 and instances[0]["instance"] == agent["instance"]
        verify_view(gui_socket)
        assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
        assert Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
        harness.pass_check("saved workspace/pane/tab automatically reconnects the same instance, PID, screen and input; spawn count remains one")

        before = int((harness.root / "progress").read_text())
        assert app in harness.children and app.pid not in harness.protected_pids
        app.kill()
        assert app.wait(timeout=10) == -signal.SIGKILL
        fixture.wait_for(lambda: int((harness.root / "progress").read_text()) >= before + 5,
                         "work continues after GUI crash")
        assert host.poll() is None
        app, gui_socket = harness.window("agent-host-after-crash")
        verify_view(gui_socket)
        assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
        assert Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
        harness.assert_protected()
        harness.pass_check("SIGKILL of the isolated GUI preserves agent progress and same-process reattachment")

        def saved_agent_tab():
            state = json.loads(harness.state_path.read_text())
            def tabs(node):
                if node["kind"] == "leaf":
                    yield from node["content"].get("surfaces", [])
                else:
                    yield from tabs(node["first"])
                    yield from tabs(node["second"])
            tab = next(tab for ws in state["workspaces"] for root in ws["surfaces"]
                       for tab in tabs(root["root_pane"]) if tab["id"] == agent_surface)
            return state, tab

        harness.close_window(app)
        state, tab = saved_agent_tab()
        sentinel = harness.root / "unexpected-shell"
        tab["agent_binding"]["epoch"] = str(uuid.uuid4())
        tab["kind"]["shell"] = "/bin/sh -c 'touch " + str(sentinel) + "; exec /bin/sh'"
        harness.state_path.write_text(json.dumps(state))
        app, gui_socket = harness.window("agent-host-wrong-epoch")
        fixture.wait_for(lambda: "Agent Host" in harness.screen(gui_socket, target), "epoch mismatch visible")
        assert f"SESSION:{agent_pid}" not in harness.screen(gui_socket, target)
        assert not sentinel.exists()
        assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
        assert host.poll() is None
        harness.pass_check("stale epoch preserves the binding and reports failure without shell fallback or duplicate launch")
        harness.close_window(app)
        state, tab = saved_agent_tab()
        tab["agent_binding"]["epoch"] = agent["epoch"]
        harness.state_path.write_text(json.dumps(state))
        app, gui_socket = harness.window("agent-host-epoch-recovered")
        verify_view(gui_socket)
        harness.send(gui_socket, target, "quit")
        host.wait(timeout=10)
        assert host.returncode == 0, host.returncode
        fixture.wait_for(lambda: not Path(f"/proc/{agent_pid}").exists(), "agent reaped")
        assert not host_socket.exists()
        harness.pass_check("natural agent exit is reaped promptly; empty Host exits and releases its socket")
        control_connection.close()
        control_connection = None
        harness.close_window(app)
        app, gui_socket = harness.window("agent-host-absent")
        fixture.wait_for(lambda: "Agent" in harness.screen(gui_socket, target), "absent Host failure visible")
        assert not host_socket.exists() and not sentinel.exists()
        assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
        assert any(tab["id"] == shell_surface for item in harness.tree(gui_socket)[0]["panes"] for tab in item["tabs"])
        harness.pass_check("missing Host leaves saved Agent tab disconnected; no Host restart, shell fallback or native resume")
        harness.close_window(app)
    finally:
        if control_connection is not None:
            if host is not None and host.poll() is None and agent_pid is not None:
                try:
                    control_connection.call("terminate", instance=agent["instance"], mode="force")
                    host.wait(timeout=10)
                except (OSError, AssertionError, subprocess.TimeoutExpired):
                    pass
            control_connection.close()
        if agent_pid is not None and birth is not None and Path(f"/proc/{agent_pid}").exists():
            # This exact fixture PID is the only process this fallback may kill.
            if Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth:
                os.kill(agent_pid, 9)
        harness.cleanup()


if __name__ == "__main__":
    main()
