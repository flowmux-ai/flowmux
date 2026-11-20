#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check fast detached descendants and execution-isolated explicit termination."""

import argparse
import ctypes
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("host_fixture", ROOT / "scripts/test-agent-host-gui.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def identity(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()
        return int(fields[19]) if fields[0] != "Z" else None
    except FileNotFoundError:
        return None


def wait(check, description):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if value := check():
            return value
        time.sleep(.02)
    raise AssertionError(description)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", default=str(ROOT / "target/debug/flowmux-daemon"))
    args = parser.parse_args()
    assert sys.platform == "linux", "Linux cgroup fixture"
    assert ctypes.CDLL(None).prctl(36, 1, 0, 0, 0) == 0, "subreaper"
    root = Path(tempfile.mkdtemp(prefix="fm-containment-", dir="/tmp"))
    print(f"ARTIFACTS: {root}", flush=True)
    env = {key: value for key, value in os.environ.items() if not key.startswith("FLOWMUX_")}
    for key, name in [("XDG_RUNTIME_DIR", "run"), ("XDG_STATE_HOME", "state"),
                      ("XDG_CONFIG_HOME", "config")]:
        (root / name).mkdir(mode=0o700)
        env[key] = str(root / name)
    program = root / "provider.py"
    program.write_text('''import os, signal, sys, time
from pathlib import Path
marker = Path(sys.argv[1])
if os.fork() == 0:
    os.setsid()
    if os.fork(): os._exit(0)
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    scope = Path('/sys/fs/cgroup') / Path('/proc/self/cgroup').read_text().split('0::', 1)[1].strip().lstrip('/')
    nested = scope / 'nested'
    nested.mkdir()
    (nested / 'cgroup.procs').write_text(str(os.getpid()))
    for fd in (0, 1, 2): os.close(fd)
    marker.write_text(str(os.getpid()))
    while True: time.sleep(1)
while not marker.exists(): time.sleep(.001)
os._exit(0)
''')
    log = (root / "host.log").open("w")
    host = subprocess.Popen([args.host, "--agent-host", "--socket", str(root / "run/host.sock")],
                            env=env, stdout=log, stderr=log)
    control = None
    owned = {}
    try:
        wait(lambda: (root / "run/host.sock").exists(), "Host startup")
        control = fixture.HostClient(root / "run/host.sock", "gui")

        def launch(argv):
            agent = control.call("launch", launch={
                "request_id": fixture.launch_request_id(), "provider": "fixture",
                "label": "containment", "argv": argv, "cwd": str(root),
                "env": list(env.items()), "size": {"rows": 24, "cols": 80}})["agent"]
            assert agent["state"] == "running", agent
            owned[agent["pid"]] = agent["process_start"]
            return agent

        other = launch(["/bin/sh", "-c", "trap '' HUP; while :; do read x; done"])
        agent = launch(["/usr/bin/python3", str(program), str(root / "child.pid")])
        wait(lambda: (root / "child.pid").exists(), "detached child identity")
        child = int((root / "child.pid").read_text())
        owned[child] = identity(child)
        assert owned[child] is not None

        def current(item):
            return next(entry for entry in control.call("list")["agents"]
                        if entry["instance"] == item["instance"])

        wait(lambda: identity(agent["pid"]) is None, "root exit")
        time.sleep(.8)
        assert current(agent)["state"] == "running", current(agent)
        child_group = Path(f"/proc/{child}/cgroup").read_text()
        assert agent["instance"] in child_group and child_group.strip().endswith(".scope/nested"), child_group
        assert child_group != Path(f"/proc/{other['pid']}/cgroup").read_text()
        print("PASS: first-observation double-fork/setsid child survives root exit in its nested execution scope", flush=True)
        control.call("terminate", instance=agent["instance"], epoch=agent["epoch"], mode="terminate")
        wait(lambda: current(agent)["force_terminate_allowed"], "explicit force grace")
        assert identity(child) == owned[child] and current(agent)["state"] == "stopping"
        control.call("terminate", instance=agent["instance"], epoch=agent["epoch"], mode="force")
        wait(lambda: current(agent)["state"] == "exited", "detached child reaped before exit")
        assert not Path(f"/proc/{child}").exists(), "Host must reap adopted detached child"
        assert identity(other["pid"]) == owned[other["pid"]]
        assert current(other)["state"] == "running"
        def adopted_zombies_gone():
            children = set()
            for task in Path(f"/proc/{host.pid}/task").iterdir():
                children.update((task / "children").read_text().split())
            for pid in children:
                stat = Path(f"/proc/{pid}/stat")
                try:
                    if stat.read_text().rsplit(") ", 1)[1].startswith("Z "):
                        return False
                except FileNotFoundError:
                    pass
            return True
        wait(adopted_zombies_gone, "first-observation exited orphans must also be reaped")
        print("PASS: graceful/force termination reaps detached child and preserves the other Agent", flush=True)
        control.call("terminate", instance=other["instance"], epoch=other["epoch"], mode="terminate")
        wait(lambda: current(other)["state"] == "exited", "other execution terminated")
        control.close()
        control = None
        assert host.wait(timeout=10) == 0
        print("PASS: empty Host retires after both execution scopes are empty", flush=True)
    finally:
        if control:
            control.close()
        for pid, birth in owned.items():
            if birth is not None and identity(pid) == birth:
                os.kill(pid, signal.SIGKILL)
        if host.poll() is None:
            host.terminate()
        host.wait(timeout=10)
        log.close()
        wait(lambda: all(identity(pid) != birth for pid, birth in owned.items()), "owned process cleanup")
        while True:
            try:
                if os.waitpid(-1, os.WNOHANG)[0] == 0:
                    break
            except ChildProcessError:
                break
        (root / "result.json").write_text(json.dumps({"remaining_owned_processes": []}) + "\n")


if __name__ == "__main__":
    main()
