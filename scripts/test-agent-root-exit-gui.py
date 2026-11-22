#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Reconnect a surviving child PTY after the managed provider root exits."""

from agent_host_protocol import SOCKET_NAME

import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import signal
import sys
from types import SimpleNamespace

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("host_fixture", ROOT / "scripts/test-agent-host-gui.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
gui_fixture = fixture.fixture


def identity(pid):
    try:
        return Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19]
    except FileNotFoundError:
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("gui", "cli", "host"):
        default = {"gui": "flowmux", "cli": "flowmuxctl", "host": "flowmux-daemon"}[name]
        parser.add_argument(f"--{name}", default=str(ROOT / "target/debug" / default))
    parser.add_argument("--protected-pid", action="append", type=int, default=[])
    args = parser.parse_args()
    for name in ("gui", "cli", "host"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    harness = gui_fixture.Harness(SimpleNamespace(gui=args.gui, cli=args.cli, protected_pid=args.protected_pid))
    host = control = None
    agent = child = birth = None
    try:
        harness.start_display()
        program = harness.root / "provider.py"
        program.write_text('''import json, os, signal, subprocess, sys, tty
from pathlib import Path
root = Path(sys.argv[1])
(root / 'starts').write_text(str(os.getpid()))
signal.signal(signal.SIGHUP, signal.SIG_IGN)
if os.fork(): os._exit(0)
(root / 'child').write_text(str(os.getpid()))
tty.setraw(0)
print('SURVIVING-CHILD:' + str(os.getpid()) + '\\r\\n한글 화면', flush=True)
pending = b''
while True:
    data = os.read(0, 4096)
    if not data: break
    pending += data.replace(b'\\r', b'\\n')
    while b'\\n' in pending:
        line, pending = pending.split(b'\\n', 1)
        if line == b'quit': sys.exit(0)
        if line == b'context':
            results = []
            for args in (['--json', 'identify'], ['notify', '--title', 'Surviving child', 'Root exited']):
                result = subprocess.run([sys.argv[2], *args], capture_output=True, text=True)
                results.append(dict(code=result.returncode, stdout=result.stdout, stderr=result.stderr))
            temporary = root / 'context-result.tmp'
            temporary.write_text(json.dumps(results))
            temporary.replace(root / 'context-result')
            continue
        print('\\r\\nACK:<' + line.decode() + '>', flush=True)
''')
        socket = harness.root / "run" / SOCKET_NAME
        log = (harness.root / "host.log").open("w")
        host = subprocess.Popen([args.host, "--agent-host", "--socket", str(socket)],
                                env=harness.env, stdout=log, stderr=log)
        harness.children.append(host)
        log.close()
        gui_fixture.wait_for(socket.exists, "isolated Host startup")
        control = fixture.HostClient(socket, "gui")
        agent = control.call("launch", launch={
            "request_id": fixture.launch_request_id(), "provider": "fixture", "label": "surviving child",
            "argv": ["/usr/bin/python3", "-u", str(program), str(harness.root), args.cli],
            "cwd": str(harness.root), "env": list(harness.env.items()),
            "size": {"rows": 24, "cols": 80}})["agent"]
        assert agent["state"] == "running", agent
        gui_fixture.wait_for(lambda: identity(agent["pid"]) is None, "provider root reaped")
        child = int((harness.root / "child").read_text())
        birth = identity(child)
        assert birth is not None
        copied_env = dict(harness.env, FLOWMUX_SOCKET_PATH=str(socket),
                          FLOWMUX_AGENT_INSTANCE_ID=agent['instance'], FLOWMUX_AGENT_HOST_EPOCH=agent['epoch'])
        for command in (['--json', 'identify'], ['notify', '--title', 'Forged', 'Copied environment']):
            forged = subprocess.run([args.cli, *command], env=copied_env, capture_output=True, text=True)
            assert forged.returncode != 0 and 'does not belong' in forged.stderr, forged
        app, gui = harness.window("surviving-child")
        if not harness.tree(gui):
            harness.rpc(gui, "workspace_create", root=str(harness.root), name="surviving child", background=False, team=False)
        gui_fixture.wait_for(lambda: harness.tree(gui) and harness.tree(gui)[0]["panes"], "terminal pane")
        pane = harness.tree(gui)[0]["panes"][0]["id"]
        for marker in ("first-한글", "reconnected-한글"):
            opened = harness.rpc(gui, "agent_view_open", pane=pane, instance=agent["instance"])
            surface = opened["surface_created"]["id"]
            gui_fixture.wait_for(lambda: f"SURVIVING-CHILD:{child}" in harness.screen(gui, pane), "child output through live VTE")
            harness.send(gui, pane, marker)
            gui_fixture.wait_for(lambda: f"ACK:<{marker}>" in harness.screen(gui, pane), "fresh child input through live VTE")
            assert identity(child) == birth and identity(agent["pid"]) is None
            assert (harness.root / "starts").read_text() == str(agent["pid"])
            current = next(a for a in control.call("list")["agents"] if a["instance"] == agent["instance"])
            assert current["state"] == "running" and current["pid"] == agent["pid"]
            harness.pass_check(f"{marker}: live GUI attaches to the same surviving child PTY after root exit without spawning again")
            result_path = harness.root / 'context-result'
            result_path.unlink(missing_ok=True)
            harness.send(gui, pane, 'context')
            gui_fixture.wait_for(result_path.exists, 'surviving child context response')
            results = json.loads(result_path.read_text())
            assert all(result['code'] == 0 for result in results), results
            context = json.loads(results[0]['stdout'])
            assert context['pane'] == pane and context['surface'] == surface, context
            notifications = control.call('list')['notifications']
            assert notifications[-1]['notification']['title'] == 'Surviving child', notifications
            assert all(item['notification']['title'] != 'Forged' for item in notifications)
            harness.pass_check(f"{marker}: surviving child resolves current GUI context and sends notifications; copied identity is rejected")
            if marker.startswith("first"):
                harness.rpc(gui, "surface_close", pane=pane, surface=surface)
                gui_fixture.wait_for(lambda: next(a for a in control.call("list")["agents"]
                    if a["instance"] == agent["instance"])["attachment"] is None, "child view detaches")
        harness.send(gui, pane, "quit")
        gui_fixture.wait_for(lambda: not Path(f"/proc/{child}").exists(), "child reaped")
        control.close()
        control = None
        assert host.wait(timeout=10) == 0
        harness.assert_protected()
        harness.pass_check("child exit is reaped and empty Host retires after root-exit reconnect")
    finally:
        try:
            if control and agent:
                try:
                    control.call("terminate", instance=agent["instance"], epoch=agent["epoch"], mode="terminate")
                    if child:
                        gui_fixture.wait_for(lambda: not Path(f"/proc/{child}").exists(), "owned child cleanup")
                finally:
                    if child and birth and identity(child) == birth:
                        os.kill(child, signal.SIGKILL)
        finally:
            if control:
                control.close()
            harness.cleanup()



if __name__ == "__main__":
    main()
