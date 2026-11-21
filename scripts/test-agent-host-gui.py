#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify same-process Host reconnection through an isolated Flowmux/VTE view.

By default the fixture launches the Host explicitly. --service-start exercises
the product's agent run command and a real Linux user service. GUI management,
opening and saved tab reconnection use product paths, without native resume.
--headless-seconds extends the live Working check after the last GUI exits.
"""

import argparse
import contextlib
import importlib.util
import json
import os
import fcntl
import pty
import select
import termios
from pathlib import Path
import re
import signal
import socket
import struct
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


def launch_request_id():
    # UUIDv7 timestamp stays fixed when replaying an evicted launch request.
    value = ((time.time_ns() // 1_000_000) << 80) | (uuid.uuid4().int & ((1 << 80) - 1))
    return str(uuid.UUID(int=(value & ~(0xf << 76) & ~(3 << 62)) | (7 << 76) | (2 << 62)))


class HostClient:
    def __init__(self, path, role):
        self.stream = socket.socket(socket.AF_UNIX)
        self.stream.settimeout(10)
        self.stream.connect(str(path))
        self.reader = self.stream.makefile("rb")
        self.identifier = 0
        self.call("hello", version=17, role=role)

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
    parser.add_argument("--service-start", action="store_true",
                        help="launch through agent run and the real systemd user manager")
    parser.add_argument("--headless-seconds", type=int, default=0,
                        help="observe continued work without a GUI for this many real seconds")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    if args.headless_seconds < 0:
        parser.error("--headless-seconds must be nonnegative")
    for name in ("gui", "cli", "host"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    harness = fixture.Harness(SimpleNamespace(gui=args.gui, cli=args.cli, protected_pid=args.protected_pid))
    harness.env["GTK_A11Y"] = "atspi"
    options = harness.root / "config/flowmux/options.json"
    options.parent.mkdir(parents=True, exist_ok=True)
    options.write_text(json.dumps({"default_shell": "/bin/sh", "auto_resume_agent_sessions": True}))
    host = None
    control_connection = None
    hook_connection = None
    agent_pid = None
    birth = None
    host_pid = None
    host_birth = None
    resize_agents = []
    host_socket = harness.root / "run/flowmux-agent-host.sock"
    try:
        harness.start_display()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = harness.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ.pop("AT_SPI_BUS_ADDRESS", None)
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)

        def label_visible(process, label):
            def walk(node):
                if (node.get_role() == Atspi.Role.LABEL
                        and node.get_name().startswith(label)
                        and node.get_state_set().contains(Atspi.StateType.SHOWING)):
                    return True
                return any(walk(child) for index in range(node.get_child_count())
                           if (child := node.get_child_at_index(index)))
            desktop = Atspi.get_desktop(0)
            return any(application.get_process_id() == process.pid and walk(application)
                       for index in range(desktop.get_child_count())
                       if (application := desktop.get_child_at_index(index)))

        app, gui_socket = harness.window("agent-host-first")
        if not args.service_start:
            with (harness.root / "host.log").open("w") as log:
                host = harness.spawn([args.host, "--agent-host", "--socket", str(host_socket)],
                                     stdout=log, stderr=log, cwd=harness.root)
            fixture.wait_for(host_socket.exists, "Agent Host socket")
        provider = harness.root / "fixture-agent.py"
        provider.write_text('''import os, select, sys, time, tty
from pathlib import Path
root = Path(sys.argv[1])
tty.setraw(0)
with (root / 'starts').open('a') as file: file.write(str(os.getpid()) + '\\n')
counter = 0
received = 0
pending = b''
pause_output = False
print('\\x1b[?1049h\\x1b[2J\\x1b[HSESSION:' + str(os.getpid()) + '\\r\\n한글 화면 e\\u0301', end='', flush=True)
while True:
    counter += 1
    (root / 'progress.tmp').write_text(str(counter))
    (root / 'progress.tmp').replace(root / 'progress')
    if not pause_output:
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
            if text == 'sync-open':
                pause_output = True
                print('\\x1b[?2026h\\x1b[5;1HSYNC-TIMEOUT:한글 e\\u0301', end='', flush=True)
                (root / 'sync-open').write_text('no more output until sync-end')
                continue
            if text == 'sync-end':
                pause_output = False
                print('\\x1b[?2026l', end='', flush=True)
            if text == 'combining-hide':
                pause_output = True
                print('\\x1b[?1049l\\x1b[2J\\x1b[H\\x1b[?2026hBOUND:e'
                      + '\\u0301' * 100_000
                      + '\\x1b[?1049h\\x1b[2J\\x1b[HCOMBINING-HIDDEN\\x1b[?2026l', end='', flush=True)
                continue
            if text == 'combining-reveal':
                print('\\x1b[?1049l', end='', flush=True)
                continue
            if text == 'combining-done':
                pause_output = False
                print('\\x1b[?1049h\\x1b[2J\\x1b[HSESSION:' + str(os.getpid())
                      + '\\r\\n한글 화면 e\\u0301', end='', flush=True)
            if text == 'title-flood':
                pause_output = True
                for index in range(1200):
                    print('\\x1b]2;' + str(index) + '한' * 20_000 + '\\x07\\x1b[22;0t', end='')
                print('\\x1b[2;1HTITLE-FLOOD-DONE', end='', flush=True)
                continue
            if text == 'hyperlink-flood':
                pause_output = True
                print('\\x1b[2J\\x1b[H', end='')
                for index in range(1200):
                    suffix = str(index) + 'x' * 60_000
                    link = ('id=small;https://example.com/' + suffix if index % 2 == 0
                            else 'id=' + suffix + ';https://example.com')
                    print('\\x1b]8;' + link + '\\x1b\\\\x', end='')
                print('\\x1b]8;;\\x1b\\\\\\x1b[2;1HLINK-FLOOD-DONE', end='', flush=True)
                continue
            if text == 'hyperlink-done':
                pause_output = False
                print('\\x1b[2J\\x1b[HSESSION:' + str(os.getpid())
                      + '\\r\\n한글 화면 e\\u0301', end='', flush=True)
            if text == 'projection-flood':
                pause_output = True
                print('\\x1b[2J\\x1b[H', end='')
                for index in range(1200):
                    print('\\x1b]8;id=' + str(index) + ';https://projection.example/'
                          + 'x' * 1000 + '\\x1b\\\\X', end='')
                print('\\x1b]8;;\\x1b\\\\\\x1b[2;1HPROJECTION-FLOOD-DONE', end='', flush=True)
                continue
            if text == 'retained-link-flood':
                pause_output = True
                print('\\x1b[3J\\x1b[2J\\x1b[H', end='')
                for index in range(10000):
                    print('\\x1b]8;id=' + str(index) + ';https://retained.example/'
                          + 'x' * 4000 + '\\x1b\\\\\\x1b[58;5;'
                          + str(index % 255) + 'mX\\u0301', end='')
                print('\\x1b]8;;\\x1b\\\\\\x1b[0m\\r\\nRETAINED-LINK-FLOOD-DONE', end='', flush=True)
                continue
            if text == 'osc-open':
                pause_output = True
                print('\\x1b]8;;https://oversized.example/' + 'x' * (256 * 1024), end='', flush=True)
                (root / 'osc-open').write_text('unterminated oversized hyperlink')
                continue
            if text == 'osc-end':
                pause_output = False
                print('\\x07\\x1b[6;1HOSC-RECOVERED:한글', end='', flush=True)
            if len(text) > 1024: text = 'bulk'
            print('\\x1b[4;1HACK:<' + text + '>\\x1b[K', end='', flush=True)
''')
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
        if args.service_start:
            launch_env = dict(harness.env)
            # Only the service-submission client uses the user's manager bus;
            # the GUI, VTE and saved state keep their isolated test session.
            launch_env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path=/run/user/{os.getuid()}/bus"
            cli = [args.cli, "--socket", str(gui_socket), "--json", "agent"]
            empty = subprocess.run(cli + ["ls"], env=launch_env, check=True, capture_output=True, text=True)
            assert json.loads(empty.stdout) == [] and not host_socket.exists()
            invalid = subprocess.run(cli + ["run", "--pane", target, "--", "/flowmux-missing-agent"],
                                     env=launch_env, capture_output=True, text=True)
            assert invalid.returncode != 0 and not host_socket.exists()
            launched = subprocess.run(cli + ["run", "--pane", target, "--cwd", str(harness.root),
                "--", "/usr/bin/python3", "-u", str(provider), str(harness.root)],
                env=launch_env, check=True, capture_output=True, text=True, timeout=30)
            result = json.loads(launched.stdout)
            agent = result["agent"]
            agent_surface = result["view"]["surface_created"]["id"]
            assert Path(args.cli).with_name("flowmux-daemon").samefile(args.host)
        control_connection = HostClient(host_socket, "relay")
        if not args.service_start:
            launch = {"request_id": launch_request_id(), "provider": "fixture", "label": "isolated Agent Host fixture",
                      "argv": ["/usr/bin/python3", "-u", str(provider), str(harness.root)],
                      "cwd": str(harness.root), "env": list(harness.env.items()) + [["TERM", "xterm-256color"]],
                      "size": {"rows": 24, "cols": 80}}
            agent = control_connection.call("launch", launch=launch)["agent"]
        agent_pid = agent["pid"]
        assert agent["state"] == "running", agent
        birth = Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19]

        def lifecycle(seq, event, **fields):
            return hook_connection.call(
                "lifecycle", instance=agent["instance"], epoch=agent["epoch"],
                pid=agent_pid, process_start=int(birth), session_id="fixture-session",
                seq=seq, event={"event": event, **fields})

        def presence():
            agents = control_connection.call("list")["agents"]
            return next(item for item in agents if item["instance"] == agent["instance"])["presence"]

        host_pid, _, _ = struct.unpack("3i", control_connection.stream.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        assert host_pid not in harness.protected_pids
        host_birth = Path(f"/proc/{host_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19]

        def host_alive():
            stat = Path(f"/proc/{host_pid}/stat")
            return stat.exists() and stat.read_text().rsplit(") ", 1)[1].split()[19] == host_birth

        if args.service_start:
            cgroup = Path(f"/proc/{host_pid}/cgroup").read_text()
            assert "flowmux-agent-host-" in cgroup and cgroup.strip().endswith(".service"), cgroup
            assert cgroup != Path(f"/proc/{app.pid}/cgroup").read_text()
            agent_cgroup = Path(f"/proc/{agent_pid}/cgroup").read_text()
            assert agent["instance"] in agent_cgroup and agent_cgroup.strip().endswith(".scope"), agent_cgroup
            assert agent_cgroup != cgroup
            listed = subprocess.run(cli + ["ls"], env=launch_env, check=True, capture_output=True, text=True)
            assert [item["instance"] for item in json.loads(listed.stdout)] == [agent["instance"]]
            harness.pass_check("agent run starts an independent user service; list and invalid launches do not start a Host")
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

        contender = HostClient(host_socket, "relay")
        try:
            source = next(a for a in control_connection.call("list")["agents"]
                          if a["instance"] == agent["instance"])
            pending = contender.call("prepare_attach", instance=agent["instance"],
                epoch=agent["epoch"], request_id=str(uuid.uuid4()), view=None,
                require_gui=True, surface=str(uuid.uuid4()), replace=source["attachment"]["key"])
            with harness.state_path.with_name("state.lock").open("a") as state_lock:
                fcntl.flock(state_lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                try:
                    contender.stream.settimeout(1)
                    contender.call("abort_attach", key=pending["key"])
                    verify_view(gui_socket)
                    current = next(a for a in control_connection.call("list")["agents"]
                                   if a["instance"] == agent["instance"])
                    assert current["attachment"]["key"] == source["attachment"]["key"]
                finally:
                    fcntl.flock(state_lock, fcntl.LOCK_UN)
        finally:
            contender.close()
        harness.pass_check("a contended state save cannot stall transfer cancellation or fresh GUI input; the original lease survives")

        hostile = (b'{"id":1,"kind":"request","verb":"ping","unused":['
                   + b'0,' * 150_000 + b'0]}\n')
        assert len(hostile) < 1024 * 1024
        for _ in range(32):
            with socket.socket(socket.AF_UNIX) as peer:
                peer.settimeout(5)
                peer.connect(str(host_socket))
                peer.sendall(hostile)
                assert peer.recv(1) == b'', "Host dispatched excessive JSON structure"
        assert host_alive()
        assert Path(f'/proc/{agent_pid}/stat').read_text().rsplit(') ', 1)[1].split()[19] == birth
        verify_view(gui_socket)
        harness.pass_check("dense sub-MiB JSON requests are rejected before dispatch; the same Agent and live VTE accept fresh Hangul input")

        narrow_provider = harness.root / "narrow-history.py"
        narrow_provider.write_text("""import os, pathlib, sys, termios, tty
root = pathlib.Path(sys.argv[1])
columns = int(sys.argv[2])
settings = termios.tcgetattr(0)
tty.setraw(0)
sys.stdout.write(('x' * columns + '\\r\\n') * (100000 // columns + 20) + 'Z\\x1b[6n')
sys.stdout.flush()
reply = b''
while not reply.endswith(b'R'): reply += os.read(0, 128)
termios.tcsetattr(0, termios.TCSANOW, settings)
(root / 'narrow-history-ready').touch()
for line in sys.stdin:
    if line.strip() == 'quit': break
    print('RESIZE-ACK:' + line.strip(), flush=True)
""")
        for initial_columns in (2, 500):
            ready = harness.root / "narrow-history-ready"
            ready.unlink(missing_ok=True)
            resize_before = int(next(line.split()[1] for line in Path(f"/proc/{host_pid}/status").read_text().splitlines()
                                     if line.startswith("VmHWM:")))
            narrow = control_connection.call("launch", launch={
                "request_id": launch_request_id(), "provider": "fixture", "label": "narrow history",
                "argv": ["/usr/bin/python3", "-u", str(narrow_provider), str(harness.root), str(initial_columns)],
                "cwd": str(harness.root), "env": list(harness.env.items()),
                "size": {"rows": 20, "cols": initial_columns}})["agent"]
            resize_agents.append(narrow)
            fixture.wait_for(ready.exists, "resize history written")
            if initial_columns == 500:
                prepared = control_connection.call("prepare_attach", instance=narrow["instance"],
                    epoch=narrow["epoch"], request_id=str(uuid.uuid4()), view=None, replace=None)
                key = prepared["key"]
                try:
                    control_connection.call("commit_attach", key=key, size={"rows": 20, "cols": 2})
                    frame = control_connection.call("frame", key=key, after=0, offset=0)["frame"]
                    assert frame["size"] == {"rows": 20, "cols": 2}, frame["size"]
                    text = frame["data"]
                    while frame["next_offset"] is not None:
                        frame = control_connection.call("frame", key=key, after=frame["sequence"],
                                                        offset=frame["next_offset"])["frame"]
                        text += frame["data"]
                    assert "Z" in text, "narrowed primary screen was lost"
                finally:
                    control_connection.call("detach", key=key)
            narrow_surface = harness.rpc(gui_socket, "agent_view_open", pane=target,
                                         instance=narrow["instance"])["surface_created"]["id"]
            token = uuid.uuid4().hex
            harness.send(gui_socket, target, token)
            fixture.wait_for(lambda: "RESIZE-ACK:" + token in harness.screen(gui_socket, target),
                             "widened Background agent accepts fresh GUI input")
            current_narrow = next(item for item in control_connection.call("list")["agents"]
                                  if item["instance"] == narrow["instance"])
            assert (current_narrow["pid"], current_narrow["process_start"]) == (narrow["pid"], narrow["process_start"])
            resize_after = int(next(line.split()[1] for line in Path(f"/proc/{host_pid}/status").read_text().splitlines()
                                    if line.startswith("VmHWM:")))
            print(f"HOST_RESIZE_PEAK_KIB initial_columns={initial_columns} before={resize_before} after={resize_after}", flush=True)
            assert resize_after - resize_before < 128 * 1024, (resize_before, resize_after)
            harness.send(gui_socket, target, "quit")
            fixture.wait_for(lambda: not Path(f"/proc/{narrow['pid']}").exists(), "narrow history agent reaped")
            # The finished agent's tab stays as an ordinary terminal; close it like any tab.
            harness.rpc(gui_socket, "surface_close", pane=target, surface=narrow_surface)
            harness.rpc(gui_socket, "surface_focus", pane=target, surface=agent_surface)
            verify_view(gui_socket)
            harness.pass_check("narrowing and reopening Background history bounds resize peak and preserves identity, fresh input and the original agent")

        pressure = harness.root / "memory-pressure.py"
        release = harness.root / "memory-release"
        pressure.write_text("""import os, pathlib, sys, time, tty
root = pathlib.Path(sys.argv[1])
tty.setraw(0)
while not (root / 'memory-start').exists():
    if (root / 'memory-release').exists(): sys.exit(0)
    time.sleep(0.05)
if (root / 'memory-release').exists(): sys.exit(0)
for index in range(256):
    print('\\x1b]2;' + str(index) + 't' * 4090 + '\\x07\\x1b[22;0t', end='')
line = ''.join('\\x1b[0;1;2;3;4:3;7;8;9;38;2;' + str(column % 255)
               + ';1;2;48;2;3;4;5;58;2;6;7;8mX' + '\\U0001d185' * 32
               for column in range(500)) + '\\r\\n'
print('\\x1b[?1049h', end='')
for index in range(100): print(line, end='')
print('\\x1b[?1049l', end='')
# At 500 columns the Host keeps 80 visible and 200 history rows; fill both once.
# Volume beyond that only measures the runner; the unit tests cover the bounds.
for index in range(320): print(line, end='')
for column in range(500):
    print('\\x1b[1;' + str(column + 1) + 'H\\x1b]8;;https://example.com/'
          + str(column) + '/' + 'l' * 4000 + '\\x1b\\\\X' + '\\U0001d185' * 32, end='')
print('\\x1b]8;;\\x1b\\\\', end='')
print('\\x1b[6n', end='', flush=True)
reply = b''
while not reply.endswith(b'R'): reply += os.read(0, 256)
(root / ('memory-ready-' + sys.argv[2])).touch()
while not (root / 'memory-release').exists(): time.sleep(0.05)
""")
        extras = []
        prepared_pressure = []
        memory_peers = contextlib.ExitStack()
        memory_before = int(next(line.split()[1] for line in Path(f"/proc/{host_pid}/status").read_text().splitlines()
                                 if line.startswith("VmRSS:")))
        try:
            for index in range(32):
                request = {"request_id": launch_request_id(), "provider": "fixture", "label": "memory pressure",
                           "argv": ["/usr/bin/python3", "-u", str(pressure), str(harness.root), str(index)],
                           "cwd": str(harness.root), "env": list(harness.env.items()),
                           "size": {"rows": 79, "cols": 499}}
                try:
                    extra = control_connection.call("launch", launch=request)["agent"]
                except AssertionError as error:
                    assert "memory budget" in str(error), error
                    break
                assert extra["state"] == "running", extra
                extras.append(extra)
                harness.log("memory-agent", pid=extra["pid"], birth=extra["process_start"])
                prepared = control_connection.call("prepare_attach", instance=extra["instance"],
                    epoch=extra["epoch"], request_id=str(uuid.uuid4()), view=None,
                    require_gui=False, replace=None)
                key = prepared["key"]
                try:
                    attached = control_connection.call("commit_attach", key=key, size={"rows": 79, "cols": 499})
                    control_connection.call("view_ready", key=key, sequence=attached["frame"]["sequence"])
                    # Width then height growth allocates the emulator's widest row cache.
                    control_connection.call("resize", key=key, size={"rows": 79, "cols": 500})
                    control_connection.call("resize", key=key, size={"rows": 80, "cols": 500})
                finally:
                    control_connection.call("detach", key=key)
            else:
                raise AssertionError("aggregate memory admission did not refuse a launch")
            assert len(extras) == 6, len(extras)
            (harness.root / 'memory-start').touch()
            # Exercise the idle deadline while pressure output parses concurrently.
            time.sleep(31)
            try:
                fixture.wait_for(lambda: all((harness.root / f"memory-ready-{index}").exists()
                                            for index in range(len(extras))),
                                 "concurrent pressure output parsed and terminal query answered", timeout=60)
            except AssertionError as error:
                # Tell a slow Host (CPU still advancing) from a stalled one.
                def host_cpu():
                    fields = Path(f"/proc/{host_pid}/stat").read_text().rsplit(')', 1)[1].split()
                    return int(fields[11]) + int(fields[12])
                before = host_cpu()
                time.sleep(2)
                ready = [index for index in range(len(extras)) if (harness.root / f"memory-ready-{index}").exists()]
                raise AssertionError(f"{error}; ready={ready} of {len(extras)}; "
                                     f"Host CPU ticks in 2s={host_cpu() - before}") from error
            # Output parsing can exceed the IPC connection's 30-second idle limit.
            control_connection.close()
            control_connection = HostClient(host_socket, "relay")
            dense = {"id": 1, "kind": "request", "verb": "ping",
                     "unused": ["x" * 31] * 30_000, "padding": ""}
            dense["padding"] = "p" * (1024 * 1024 - 1 - len(json.dumps(dense, separators=(',', ':'))))
            dense_line = (json.dumps(dense, separators=(',', ':')) + '\n').encode()
            assert len(dense_line) == 1024 * 1024
            admitted = []
            pool_counts = []
            for endpoint, limit in ((host_socket, 256), (Path(str(host_socket) + '.ctl'), 16)):
                count = 0
                for _ in range(limit + 1):
                    peer = memory_peers.enter_context(socket.socket(socket.AF_UNIX))
                    peer.settimeout(5)
                    peer.connect(str(endpoint))
                    reader = memory_peers.enter_context(peer.makefile('rb'))
                    try:
                        peer.sendall(b'{"id":0,"kind":"request","verb":"ping"}\n')
                        response = reader.readline()
                    except (BrokenPipeError, ConnectionResetError):
                        break
                    if not response:
                        break
                    assert 'pong' in json.loads(response), response
                    peer.sendall(dense_line)
                    assert 'pong' in json.loads(reader.readline())
                    admitted.append((peer, reader))
                    count += 1
                else:
                    raise AssertionError(f'connection admission did not bound {endpoint}')
                assert count > 0, (endpoint, count)
                pool_counts.append(count)
            assert pool_counts[1] == 16, pool_counts
            # Retain the previous 1 MiB line while the next incomplete line fills.
            for peer, _ in admitted:
                peer.sendall(dense_line[:-1])
            for extra in extras:
                prepared = control_connection.call('prepare_attach', instance=extra['instance'],
                    epoch=extra['epoch'], request_id=str(uuid.uuid4()), view=None,
                    require_gui=True, surface=str(uuid.uuid4()), replace=None)
                prepared_pressure.append(prepared)
                assert prepared['frame']['next_offset'] is not None, prepared['frame']
            # Parse at the byte/structure boundary with every snapshot and peer retained.
            for peer, reader in admitted:
                peer.sendall(b'\n')
                assert 'pong' in json.loads(reader.readline())
                peer.sendall(dense_line[:-1])
            for prepared in prepared_pressure:
                frame = prepared['frame']
                continued = control_connection.call('frame', key=prepared['key'],
                    after=frame['sequence'], offset=frame['next_offset'])['frame']
                assert continued['sequence'] == frame['sequence']
            memory_after = int(next(line.split()[1] for line in Path(f"/proc/{host_pid}/status").read_text().splitlines()
                                    if line.startswith("VmRSS:")))
            memory_peak = int(next(line.split()[1] for line in Path(f"/proc/{host_pid}/status").read_text().splitlines()
                                   if line.startswith("VmHWM:")))
            assert memory_peak < 3 * 1024 * 1024, (memory_before, memory_after, memory_peak)
            print(f"HOST_BUDGET_RSS_KIB before={memory_before} after={memory_after} peak={memory_peak} "
                  f"executions={len(extras) + 1} retained_transfers={len(prepared_pressure)} "
                  f"regular_pressure_peers={pool_counts[0]} control_pressure_peers={pool_counts[1]}", flush=True)
            verify_view(gui_socket)
            assert not (harness.root / f"memory-ready-{len(extras)}").exists()
        finally:
            release.touch()
            memory_peers.close()
            for prepared in prepared_pressure:
                control_connection.call('abort_attach', key=prepared['key'])
        def pressure_ended():
            states = {item["instance"]: item["state"] for item in control_connection.call("list")["agents"]}
            return all(states.get(item["instance"]) == "exited" for item in extras)
        fixture.wait_for(pressure_ended, "memory pressure agents naturally reaped", timeout=15)
        request["request_id"] = launch_request_id()
        reused = control_connection.call("launch", launch=request)["agent"]
        fixture.wait_for(lambda: not Path(f"/proc/{reused['pid']}").exists(), "released memory admission reused")
        verify_view(gui_socket)
        harness.pass_check("concurrent dual-screen, combining, saved-title and retained-transfer pressure stays within the Host budget; admission, live input and reservation reuse work")

        harness.send(gui_socket, target, 'sync-open')
        fixture.wait_for(lambda: (harness.root / 'sync-open').exists(), 'unterminated synchronized output')
        fixture.wait_for(lambda: 'SYNC-TIMEOUT:한글 e' in harness.screen(gui_socket, target),
                         'Host expires synchronized output without further provider bytes')
        harness.send(gui_socket, target, 'sync-end')
        fixture.wait_for(lambda: 'ACK:<sync-end>' in harness.screen(gui_socket, target),
                         'input after synchronized output expiry')
        assert Path(f'/proc/{agent_pid}/stat').read_text().rsplit(') ', 1)[1].split()[19] == birth
        harness.pass_check('unterminated synchronized output reaches live VTE without further output or process replacement')

        harness.send(gui_socket, target, 'combining-hide')
        fixture.wait_for(lambda: 'COMBINING-HIDDEN' in harness.screen(gui_socket, target),
                         'synchronized batch switches away from a heavily combined cell')
        harness.send(gui_socket, target, 'combining-reveal')
        fixture.wait_for(lambda: 'BOUND:e' in harness.screen(gui_socket, target), 'hidden main screen restored')
        line = next(line for line in harness.screen(gui_socket, target).splitlines() if 'BOUND:e' in line)
        # VTE has its own smaller combining limit; inspect the Host snapshot too.
        assert 0 < line.count('\u0301') <= 32, repr(line[:200])
        source = next(item for item in control_connection.call('list')['agents']
                      if item['instance'] == agent['instance'])
        prepared = control_connection.call('prepare_attach', instance=agent['instance'],
            epoch=agent['epoch'], request_id=str(uuid.uuid4()), view=None,
            require_gui=True, surface=str(uuid.uuid4()), replace=source['attachment']['key'])
        try:
            assert prepared['frame']['next_offset'] is None
            assert prepared['frame']['data'].count('\u0301') == 32
        finally:
            control_connection.call('abort_attach', key=prepared['key'])
        harness.send(gui_socket, target, 'combining-done')
        verify_view(gui_socket)
        harness.pass_check('synchronized Unicode flood stays bounded on the hidden screen and live VTE accepts fresh input')

        def host_rss_kib():
            return int(next(line for line in Path(f'/proc/{host_pid}/status').read_text().splitlines()
                            if line.startswith('VmRSS:')).split()[1])

        before_rss = host_rss_kib()
        harness.send(gui_socket, target, 'hyperlink-flood')
        fixture.wait_for(lambda: 'LINK-FLOOD-DONE' in harness.screen(gui_socket, target),
                         'Host drains repeated oversized hyperlink URIs and IDs', timeout=45)
        after_rss = host_rss_kib()
        print(f'HYPERLINK_RSS_KIB before={before_rss} after={after_rss}', flush=True)
        assert after_rss - before_rss < 32 * 1024, (before_rss, after_rss)
        harness.send(gui_socket, target, 'hyperlink-done')
        verify_view(gui_socket)
        harness.pass_check('oversized hyperlink URI and ID flood stays below 32 MiB RSS growth and fresh input works')

        harness.send(gui_socket, target, 'projection-flood')
        fixture.wait_for(lambda: 'PROJECTION-FLOOD-DONE' in harness.screen(gui_socket, target),
                         'live VTE receives repeated valid hyperlinks', timeout=45)
        source = next(item for item in control_connection.call('list')['agents']
                      if item['instance'] == agent['instance'])
        prepared = control_connection.call('prepare_attach', instance=agent['instance'],
            epoch=agent['epoch'], request_id=str(uuid.uuid4()), view=None,
            require_gui=True, surface=str(uuid.uuid4()), replace=source['attachment']['key'])
        try:
            frame = prepared['frame']
            chunks = [frame['data']]
            while frame['next_offset'] is not None:
                assert len(chunks) < 8, 'projection exceeded 512 KiB in this fixture'
                frame = control_connection.call('frame', key=prepared['key'],
                    after=frame['sequence'], offset=frame['next_offset'])['frame']
                chunks.append(frame['data'])
            projected = ''.join(chunks)
            assert len(projected.encode()) < 512 * 1024
            uri = 'https://projection.example/' + 'x' * 1000
            assert 0 < projected.count(uri) * len(uri) <= 256 * 1024
            assert 'PROJECTION-FLOOD-DONE' in projected
        finally:
            control_connection.call('abort_attach', key=prepared['key'])
        harness.send(gui_socket, target, 'hyperlink-done')
        verify_view(gui_socket)
        assert Path(f'/proc/{agent_pid}/stat').read_text().rsplit(') ', 1)[1].split()[19] == birth
        harness.pass_check('valid hyperlink projection stays bounded while live VTE and fresh input preserve the same process')

        before_rss = host_rss_kib()
        harness.send(gui_socket, target, 'retained-link-flood')
        fixture.wait_for(lambda: 'RETAINED-LINK-FLOOD-DONE' in harness.screen(gui_socket, target),
                         'Host retains bounded valid links with combining and underline attributes', timeout=45)
        after_rss = host_rss_kib()
        print(f'RETAINED_LINK_RSS_KIB before={before_rss} after={after_rss}', flush=True)
        assert after_rss - before_rss < 24 * 1024, (before_rss, after_rss)
        harness.send(gui_socket, target, 'hyperlink-done')
        verify_view(gui_socket)
        harness.pass_check('retained valid link flood stays below 24 MiB RSS growth and the same process accepts fresh input')

        before_rss = host_rss_kib()
        harness.send(gui_socket, target, 'title-flood')
        fixture.wait_for(lambda: 'TITLE-FLOOD-DONE' in harness.screen(gui_socket, target),
                         'Host drains repeated title pushes', timeout=45)
        after_rss = host_rss_kib()
        print(f'TITLE_RSS_KIB before={before_rss} after={after_rss}', flush=True)
        assert after_rss - before_rss < 32 * 1024, (before_rss, after_rss)
        harness.send(gui_socket, target, 'hyperlink-done')
        verify_view(gui_socket)
        harness.pass_check('title stack flood stays below 32 MiB RSS growth and fresh input works')

        harness.send(gui_socket, target, 'osc-open')
        fixture.wait_for(lambda: (harness.root / 'osc-open').exists(), 'oversized unterminated OSC')
        harness.send(gui_socket, target, 'osc-end')
        fixture.wait_for(lambda: 'OSC-RECOVERED:한글' in harness.screen(gui_socket, target),
                         'live VTE recovers after oversized OSC')
        fixture.wait_for(lambda: 'ACK:<osc-end>' in harness.screen(gui_socket, target),
                         'input after oversized OSC')
        assert 'oversized.example' not in harness.screen(gui_socket, target)
        harness.pass_check('oversized unterminated OSC is discarded and live VTE resumes output and input')

        def current_agent():
            return next(item for item in control_connection.call("list")["agents"]
                        if item["instance"] == agent["instance"])

        fixture.wait_for(lambda: current_agent()["view"], "owning GUI publishes its live location")
        source_key = current_agent()["attachment"]["key"]
        # test-agent-ready-gui.py --replacement loses and expires a real relay in this state.
        for outcome in ("abort",):
            candidate = HostClient(host_socket, "relay")
            try:
                request_id = str(uuid.uuid4())
                candidate_surface = str(uuid.uuid4())
                def prepare_candidate():
                    try:
                        return candidate.call("prepare_attach", instance=agent["instance"],
                            epoch=agent["epoch"], request_id=request_id, view=None,
                            require_gui=True, surface=candidate_surface, replace=source_key)
                    except AssertionError as error:
                        # Busy means this request was not started; wait for the prior EOF cleanup.
                        assert "another attachment is being prepared" in str(error), error
                        return None
                prepared = fixture.wait_for(prepare_candidate, "replacement reservation after prior cleanup")
                key = prepared["key"]
                source_size = prepared["frame"]["size"]
                attached = candidate.call("commit_attach", key=key, size={"rows": 12, "cols": 40})
                candidate.call("resize", key=key, size={"rows": 16, "cols": 60})
                assert attached["frame"]["size"] == source_size, (source_size, attached["frame"]["size"])
                frame_size = candidate.call("frame", key=key, after=0, offset=0)["frame"]["size"]
                assert frame_size == source_size, (source_size, frame_size)
                assert current_agent()["attachment"]["key"] == source_key
                location = current_agent()["view"]
                assert location and (location["pane"], location["surface"]) == (target, agent_surface)
                verify_view(gui_socket)
                candidate.call("abort_attach", key=key)
                verify_view(gui_socket)
                assert current_agent()["attachment"]["key"] == source_key
                assert Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
                assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
                harness.pass_check(f"replacement {outcome} before ready preserves the original GUI location, input and exact execution")
            finally:
                candidate.close()
        # Use a real relay/PTY and admitting GUI peer, but deliberately lose the
        # final GUI response after Host ownership has committed.
        for outcome in ("disconnect", "timeout", "query-timeout"):
            source_key = current_agent()["attachment"]["key"]
            surface, token = str(uuid.uuid4()), str(uuid.uuid4())
            admission_path = harness.root / f"admission-{outcome}.sock"
            listener = socket.socket(socket.AF_UNIX)
            listener.bind(str(admission_path))
            listener.listen(1)
            listener.settimeout(10)
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
            relay_env = dict(harness.env, FLOWMUX_SOCKET_PATH=str(admission_path),
                             FLOWMUX_SURFACE_ID=surface)
            relay_log = (harness.root / f"ack-{outcome}.log").open("w")
            admitting_gui = HostClient(host_socket, "gui")
            relay = subprocess.Popen([args.cli, "--socket", str(host_socket), "agent-relay",
                "--instance", agent["instance"], "--epoch", agent["epoch"], "--require-gui",
                "--replace-lease", source_key["lease"], "--replace-generation", str(source_key["generation"]),
                "--ready-token", token], stdin=slave, stdout=slave, stderr=relay_log,
                start_new_session=True, env=relay_env)
            os.close(slave)
            response_stream = None
            try:
                response_stream, _ = listener.accept()
                response_stream.settimeout(10)
                with response_stream.makefile("rb") as reader:
                    request = json.loads(reader.readline())
                assert request["verb"] == "agent_view_ready" and request["token"] == token, request
                relay_birth = Path(f"/proc/{relay.pid}/stat").read_text().rsplit(") ", 1)[1].split()[19]
                # This mock GUI must prepare the protocol-15 durable destination too.
                kind = {"type": "terminal", "shell": None, "cwd": str(harness.root)}
                destination_workspace = str(uuid.uuid4())
                tab = {"id": surface, "title": "Lost ACK fixture", "kind": kind,
                       "agent_binding": {"instance": agent["instance"], "epoch": agent["epoch"]}}
                checkpoint = harness.state_path.with_suffix(
                    f".agent-view-prepared.{agent['epoch']}.{agent['instance']}.json")
                prepared = {"surface": surface,
                    "lease": [request["key"]["lease"], request["key"]["generation"]],
                    "owner": {"instance_id": str(uuid.uuid4()), "pid": os.getpid()},
                    "window_save_id": None,
                    "snapshot": {"schema_version": 4,
                        "last_saved": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                        "workspaces": [{"id": destination_workspace, "name": "Lost ACK fixture",
                            "location": {"type": "local", "root_dir": str(harness.root)},
                            "surfaces": [{"id": str(uuid.uuid4()), "kind": kind, "title": "Fixture",
                                "root_pane": {"kind": "leaf", "id": str(uuid.uuid4()),
                                    "content": {"type": "tabs", "active": surface, "surfaces": [tab]}}}]}]}}
                temporary = checkpoint.with_suffix(".tmp")
                with os.fdopen(os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as file:
                    json.dump(prepared, file)
                temporary.replace(checkpoint)
                if outcome == "disconnect":
                    original_checkpoint = checkpoint.read_bytes()
                    original_state = harness.state_path.read_bytes()
                    authority = harness.state_path.with_suffix(f".agent-views.{agent['epoch']}.json")
                    original_authority = authority.read_bytes()
                    for fault, expected in (("bytes", "32 MiB file limit"), ("structure", "JSON structure limit")):
                        if fault == "bytes":
                            with checkpoint.open("ab") as file:
                                for _ in range(512):
                                    file.write(b" " * (64 * 1024))
                        else:
                            checkpoint.write_text(json.dumps(dict(prepared, ignored=[0] * (128 * 1024 + 1))))
                        try:
                            admitting_gui.call("gui_view_ready", key=request["key"], surface=surface,
                                sequence=request["sequence"], relay_pid=relay.pid, relay_start=int(relay_birth))
                            raise RuntimeError("oversized checkpoint took over the original relay")
                        except AssertionError as error:
                            assert expected in str(error), error
                        assert harness.state_path.read_bytes() == original_state
                        assert authority.read_bytes() == original_authority
                        assert current_agent()["attachment"]["key"] == source_key
                        assert Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
                        verify_view(gui_socket)
                        harness.pass_check(f"oversized {fault} destination checkpoint preserves saved source, authority, exact execution and fresh VTE input")
                    checkpoint.write_bytes(original_checkpoint)
                admitting_gui.call("gui_view_ready", key=request["key"], surface=surface,
                    sequence=request["sequence"], relay_pid=relay.pid, relay_start=int(relay_birth))
                assert not checkpoint.exists(), "Host did not consume the destination checkpoint"
                assert current_agent()["attachment"]["key"] == request["key"]
                assert current_agent()["attachment"]["ready"]
                if outcome == "disconnect":
                    response_stream.close()
                    response_stream = None
                # Withhold timeout ACK too; the relay must observe the committed
                # lease without retransmitting admission or losing fresh input.
                if outcome == "query-timeout":
                    # Only the isolated fixture Host pauses: lose both the GUI ACK
                    # and the first registry response while retaining the relay lease.
                    os.kill(host_pid, signal.SIGSTOP)
                    try:
                        time.sleep(6.8)
                        assert relay.poll() is None, (outcome, relay_log.name)
                    finally:
                        os.kill(host_pid, signal.SIGCONT)
                else:
                    time.sleep(3.5)
                assert relay.poll() is None, (outcome, relay_log.name)
                marker = "lost-ack-한글-" + uuid.uuid4().hex[:6]
                os.write(master, (marker + "\r").encode())
                received = bytearray()
                def input_round_tripped():
                    if select.select([master], [], [], .05)[0]:
                        received.extend(os.read(master, 65536))
                    # Wide cells can insert style changes between Hangul glyphs.
                    text = re.sub(rb"\x1b\[[0-?]*[ -/]*[@-~]", b"", received)
                    return ("ACK:<" + marker + ">").encode() in text
                fixture.wait_for(input_round_tripped, "same relay accepts input after lost admission ACK")
                assert Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
                assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
                fixture.wait_for(lambda: all(tab["id"] != agent_surface
                    for ws in harness.tree(gui_socket) for pane in ws["panes"] for tab in pane["tabs"]),
                    "old GUI view closes after committed move")
                harness.pass_check(f"post-commit admission ACK {outcome} preserves the exact relay, fresh Hangul input and execution without retry")
            finally:
                if response_stream is not None:
                    response_stream.close()
                listener.close()
                os.close(master)
                if relay.poll() is None:
                    relay.terminate()
                relay.wait(timeout=10)
                relay_log.close()
                admitting_gui.close()
            fixture.wait_for(lambda: current_agent()["attachment"] is None, "lost-ACK relay detaches")
            agent_surface = harness.rpc(gui_socket, "agent_view_open", pane=target,
                instance=agent["instance"])["surface_created"]["id"]
            verify_view(gui_socket)

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
        # The input stress phase can outlast the RPC server's idle timeout.
        control_connection.close()
        control_connection = HostClient(host_socket, "relay")
        hook_connection = HostClient(host_socket, "hook")
        assert lifecycle(1, "turn_started", status_text="Working")["applied"]
        assert lifecycle(2, "wait_started", reason="approval", item_id="approval-1",
                         status_text="Approval required")["applied"]
        waiting = presence()
        assert waiting["status"] == "blocked" and len(waiting["pending_waits"]) == 1, waiting
        before = int((harness.root / "progress").read_text())
        harness.close_window(app)
        fixture.wait_for(lambda: int((harness.root / "progress").read_text()) >= before + 5, "work continues with GUI closed")
        assert host_alive()
        assert Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
        shell_stat = Path(f"/proc/{shell_pid}/stat")
        fixture.wait_for(lambda: not shell_stat.exists() or shell_stat.read_text().rsplit(") ", 1)[1].split()[19] != shell_birth,
                         "ordinary GUI-owned shell exits")
        harness.pass_check("closing the isolated GUI leaves the same agent/PTY running and progressing")
        harness.pass_check("ordinary terminal exits with GUI while managed Agent continues")
        assert presence() == waiting
        assert lifecycle(3, "wait_resolved", item_id="approval-1")["applied"]
        assert lifecycle(4, "turn_stopped", status_text="Completed")["applied"]
        completed = presence()
        assert completed["activity"] == "idle" and not completed.get("pending_waits"), completed
        revision = control_connection.call("list")["revision"]
        assert not lifecycle(4, "turn_stopped", status_text="Completed")["applied"]
        assert control_connection.call("list")["revision"] == revision
        assert lifecycle(5, "turn_started", status_text="Working")["applied"]
        assert presence()["status"] == "working"
        harness.pass_check("Host preserves approval waits after GUI exit and applies lifecycle transitions without a GUI; duplicate Stop is inert")
        if args.headless_seconds:
            started = time.monotonic()
            counter = int((harness.root / "progress").read_text())
            samples = harness.root / "headless-duration.jsonl"
            with samples.open("w", buffering=1) as evidence:
                while time.monotonic() - started < args.headless_seconds:
                    remaining = args.headless_seconds - (time.monotonic() - started)
                    time.sleep(min(5, max(0, remaining)))
                    current = current_agent()
                    progress = int((harness.root / "progress").read_text())
                    assert host_alive() and current["state"] == "running", current
                    assert (current["pid"], current["process_start"]) == (agent_pid, int(birth))
                    assert current["view"] is None and current["attachment"] is None, current
                    assert current["presence"]["status"] == "working", current
                    assert Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
                    assert progress > counter, (counter, progress)
                    assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
                    harness.assert_protected()
                    evidence.write(json.dumps({"elapsed_seconds": time.monotonic() - started,
                        "pid": agent_pid, "birth": birth, "host_pid": host_pid,
                        "host_birth": host_birth, "progress": progress}) + "\n")
                    counter = progress
            # The hook socket was idle during this read-only observation period.
            hook_connection.close()
            hook_connection = HostClient(host_socket, "hook")
            harness.pass_check(f"headless Working execution survives {args.headless_seconds} real seconds with same PID/birth, continuous progress and one spawn")
        blocked = subprocess.run(
            [args.cli, "--socket", str(host_socket), "agent-relay", "--require-gui",
             "--instance", agent["instance"], "--epoch", agent["epoch"]],
            env={**harness.env, "FLOWMUX_SURFACE_ID": agent_surface},
            stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=10)
        assert blocked.returncode != 0 and "GUI management" in blocked.stderr, blocked
        assert f"SESSION:{agent_pid}" not in blocked.stdout
        assert host_alive()
        assert Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
        harness.pass_check("GUI relay refuses attachment without a registered management connection and preserves execution")
        app, gui_socket = harness.window("agent-host-reopened")
        instances = control_connection.call("list")["agents"]
        assert len(instances) == len(extras) + len(resize_agents) + 2, instances
        running = [item for item in instances if item["state"] == "running"]
        assert len(running) == 1 and running[0]["instance"] == agent["instance"]
        assert running[0]["presence"]["status"] == "working"
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
        assert host_alive()
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
        # RUST_BACKTRACE can scroll relay stderr out of view; inspect the visible failure UI.
        fixture.wait_for(lambda: label_visible(app, "Agent connection failed. Reopen from Agents to retry."),
                         "epoch mismatch visible")
        assert f"SESSION:{agent_pid}" not in harness.screen(gui_socket, target)
        assert not sentinel.exists()
        assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
        assert host_alive()
        harness.pass_check("stale epoch preserves the binding and reports failure without shell fallback or duplicate launch")
        harness.close_window(app)
        state, tab = saved_agent_tab()
        tab["agent_binding"]["epoch"] = agent["epoch"]
        harness.state_path.write_text(json.dumps(state))
        app, gui_socket = harness.window("agent-host-epoch-recovered")
        verify_view(gui_socket)
        hook_connection.close()
        hook_connection = HostClient(host_socket, "hook")
        assert lifecycle(6, "wait_started", reason="approval", item_id="preserved-approval",
                         status_text="Approval required")["applied"]
        waits = presence()["pending_waits"]
        for seq in range(7, 307):
            if not lifecycle(seq, "wait_resolved", item_id=f"{seq}:" + "x" * 60_000)["applied"]:
                break
        else:
            raise AssertionError("Host retained unbounded lifecycle IDs")
        unknown = presence()
        assert unknown["status"] == "unknown" and unknown["pending_waits"] == waits, unknown
        assert not unknown.get("turn_outcome"), unknown
        assert not lifecycle(400, "turn_stopped", status_text="Completed")["applied"]
        assert presence() == unknown
        fixture.wait_for(lambda: label_visible(app, "Observation limit reached; execution continues"),
                         "Agents displays lifecycle observation uncertainty")
        harness.close_window(app)
        app, gui_socket = harness.window("agent-host-observation-limit-reopened")
        fixture.wait_for(lambda: label_visible(app, "Observation limit reached; execution continues"),
                         "reopening preserves lifecycle observation uncertainty")
        assert presence() == unknown
        verify_view(gui_socket)
        assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
        assert Path(f"/proc/{agent_pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
        harness.pass_check("lifecycle memory overflow preserves waits, the same process and fresh input; visible uncertainty blocks false completion")
        for session_id, applied in [("fixture-session", False), ("replacement-session", True)]:
            response = hook_connection.call(
                "native_session", instance=agent["instance"], epoch=agent["epoch"],
                pid=agent_pid, process_start=int(birth), session_id=session_id,
                seq=500, event={"event": "started", "status": "idle"})
            assert response["applied"] == applied, response
        response = hook_connection.call(
            "lifecycle", instance=agent["instance"], epoch=agent["epoch"],
            pid=agent_pid, process_start=int(birth), session_id="replacement-session",
            seq=501, event={"event": "turn_started", "status_text": "Working"})
        assert response["applied"] and presence()["status"] == "working", response
        harness.pass_check("duplicate native start cannot erase uncertain work; a new native session restores lifecycle observation")
        harness.send(gui_socket, target, "quit")
        fixture.wait_for(lambda: not Path(f"/proc/{agent_pid}").exists(), "agent reaped")
        harness.pass_check("natural agent exit is reaped promptly")
        control_connection.close()
        control_connection = None

        def open_tabs():
            return [tab["id"] for item in harness.tree(gui_socket)[0]["panes"] for tab in item["tabs"]]

        # Like an agent typed into a shell: when it exits, its tab stays as a terminal.
        fixture.wait_for(lambda: saved_agent_tab()[1].get("agent_binding") is None, "finished agent tab becomes ordinary")
        assert {agent_surface, shell_surface} <= set(open_tabs())
        assert (harness.root / "starts").read_text().splitlines() == [str(agent_pid)]
        harness.pass_check("a finished agent's tab continues as an ordinary terminal; no Host restart or relaunch")
        harness.close_window(app)
        # The window's terminals end ten seconds after it; the Host they kept then exits.
        if host is not None:
            host.wait(timeout=40)
            assert host.returncode == 0, host.returncode
        else:
            deadline = time.monotonic() + 40
            while host_alive() or host_socket.exists():
                assert time.monotonic() < deadline, "empty service Host exits"
                time.sleep(.1)
        assert not host_socket.exists()
        harness.pass_check("the Host exits and releases its socket once its window's terminals are gone")
    finally:
        if hook_connection is not None:
            hook_connection.close()
        if control_connection is not None and host_socket.exists():
            for execution in resize_agents:
                try:
                    control_connection.call("terminate", instance=execution["instance"],
                                            epoch=execution["epoch"], mode="terminate")
                except (OSError, AssertionError):
                    pass
        if args.service_start and control_connection is None and host_socket.exists():
            # A launch can succeed even if opening the tab loses its reply.
            # Recover only this isolated Host's fixture executions for cleanup.
            try:
                control_connection = HostClient(host_socket, "relay")
                for execution in control_connection.call("list")["agents"]:
                    assert execution["cwd"] == str(harness.root), execution
                    control_connection.call("terminate", instance=execution["instance"], epoch=execution["epoch"], mode="terminate")
            except (OSError, AssertionError):
                pass
        if control_connection is not None:
            if agent_pid is not None and host_socket.exists():
                try:
                    control_connection.call("terminate", instance=agent["instance"], epoch=agent["epoch"], mode="terminate")
                    if host is not None:
                        host.wait(timeout=10)
                    else:
                        fixture.wait_for(lambda: not host_socket.exists(), "fixture service cleanup", timeout=10)
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
