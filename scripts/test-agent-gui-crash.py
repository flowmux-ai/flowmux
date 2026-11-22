#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify managed execution survives an actual isolated GUI crash on Linux/macOS,
and that its saved tab becomes an ordinary terminal once no Host remains.

Linux needs a display and the real systemd user bus; macOS uses launchd.
Only GUI processes created here are killed. Logs and state remain under /tmp.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import uuid

_protocol = importlib.util.spec_from_file_location(
    "agent_host_protocol", Path(__file__).resolve().with_name("agent_host_protocol.py"))
agent_host_protocol = importlib.util.module_from_spec(_protocol)
_protocol.loader.exec_module(agent_host_protocol)


def wait_for(check, description, timeout=30):
    deadline = time.monotonic() + timeout
    while True:
        result = check()
        if result:
            return result
        assert time.monotonic() < deadline, description
        time.sleep(.05)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    args = parser.parse_args()
    binary = Path(args.gui).resolve(strict=True)
    cli = binary.with_name("flowmuxctl")
    root = Path(tempfile.mkdtemp(prefix="fm-crash-", dir="/tmp"))
    print(f"ARTIFACTS={root}", flush=True)
    env = {k: v for k, v in os.environ.items()
           if not k.startswith("FLOWMUX_") and k not in
           ("CODEX_THREAD_ID", "CODEX_SESSION_ID", "CLAUDECODE", "FLATPAK_ID")}
    for key, directory in [("FLOWMUX_RUNTIME_DIR", "run"), ("XDG_RUNTIME_DIR", "run"),
                           ("XDG_CONFIG_HOME", "config"), ("XDG_STATE_HOME", "state"),
                           ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache")]:
        path = root / directory
        path.mkdir(mode=0o700, exist_ok=True)
        env[key] = str(path)
    env.update(SHELL="/bin/sh", GSK_RENDERER="cairo", FLOWMUX_BUNDLED_CLI_PATH=str(cli))
    marker = root / "state/flowmux/welcome-seen"
    marker.parent.mkdir(parents=True)
    marker.touch()
    marker.with_name("installed-skill-version").write_text("9999.0.0")
    options = root / "config/flowmux/options.json"
    options.parent.mkdir(parents=True)
    options.write_text(json.dumps({"default_shell": "/bin/sh", "auto_resume_agent_sessions": True}))
    provider = root / "provider.py"
    provider.write_text(r'''import os, select, sys, tty
from pathlib import Path
root = Path(sys.argv[1])
tty.setraw(0)
with (root / 'starts').open('a') as file: file.write(str(os.getpid()) + '\n')
pending = b''
counter = 0
print('SESSION:' + str(os.getpid()) + '\r\n한글 화면', flush=True)
while not (root / 'release').exists():
    counter += 1
    (root / 'progress.tmp').write_text(str(counter))
    (root / 'progress.tmp').replace(root / 'progress')
    if select.select([0], [], [], .1)[0]:
        data = os.read(0, 16384)
        if not data: break
        pending += data.replace(b'\r', b'\n')
        while b'\n' in pending:
            line, pending = pending.split(b'\n', 1)
            if line == b'quit': sys.exit(0)
            print('\r\nACK:<' + line.decode() + '>', flush=True)
''')
    children = []
    agent = None
    stream = None
    reader = None
    host_socket = root / "run/flowmux-agent-host.sock"

    def rpc(path, verb, **fields):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(10)
            connection.connect(str(path))
            connection.sendall((json.dumps({"id": 1, "kind": "request", "verb": verb, **fields}) + "\n").encode())
            with connection.makefile("rb") as response:
                result = json.loads(response.readline())
        assert "error" not in result, result
        return result

    def window():
        with (root / f"gui-{len(children)}.log").open("w") as log:
            app = subprocess.Popen([str(binary)], cwd=root, env=env, stdout=log, stderr=log)
        children.append(app)
        path = root / f"run/flowmux-{app.pid}.sock"

        def ready():
            assert app.poll() is None, f"GUI exited; see {root}"
            return path.exists() and rpc(path, "ping")

        wait_for(ready, "GUI startup")
        return app, path

    def host_call(op):
        stream.sendall((json.dumps({"id": 1, "kind": "request", "verb": "agent_host",
                                   "request": {"op": op, **({"version": agent_host_protocol.PROTOCOL_VERSION, "role": "observer"} if op == "hello" else {})}}) + "\n").encode())
        response = json.loads(reader.readline())
        assert response["id"] == 1, response
        assert "error" not in response, response
        return response["agent_host"]["response"]

    def screen(path, pane):
        try:
            return rpc(path, "pane_read_screen", pane=pane)["screen_contents"]["text"]
        except AssertionError as error:
            # Saved layout appears before its GTK terminal widget is restored.
            response = error.args[0]
            if isinstance(response, dict) and response.get("error", {}).get("message") == f"pane not found: {pane}":
                return None
            raise

    try:
        app, path = window()
        rpc(path, "workspace_create", root=str(root), name="GUI crash fixture", background=False, team=False)
        workspace = rpc(path, "workspace_tree")["tree"]["workspaces"][0]
        pane = workspace["panes"][0]["id"]
        wait_for(lambda: isinstance(screen(path, pane), str), "terminal ready")
        launch_env = dict(env, FLOWMUX_CLOSE_AGENTS="keep")
        if sys.platform == "linux":
            launch_env["DBUS_SESSION_BUS_ADDRESS"] = f"unix:path=/run/user/{os.getuid()}/bus"
        result = subprocess.run([str(cli), "--socket", str(path), "--json", "agent", "run",
                                 "--pane", pane, "--cwd", str(root), "--", sys.executable,
                                 "-u", str(provider), str(root)], env=launch_env,
                                capture_output=True, text=True, timeout=30)
        assert result.returncode == 0, (result.stdout, result.stderr)
        launched = json.loads(result.stdout)
        agent = launched["agent"]
        surface = launched["view"]["surface_created"]["id"]
        stream = socket.socket(socket.AF_UNIX)
        stream.settimeout(10)
        stream.connect(str(host_socket))
        reader = stream.makefile("rb")
        host_call("hello")

        def verify_view():
            wait_for(lambda: any(p["id"] == pane and any(t["id"] == surface for t in p["tabs"])
                                 for w in rpc(path, "workspace_tree")["tree"]["workspaces"]
                                 for p in w["panes"]), "saved pane and managed tab restored")
            wait_for(lambda: f"SESSION:{agent['pid']}" in (screen(path, pane) or ""), "same process screen")
            assert "한글 화면" in screen(path, pane)
            token = "한글 입력 " + uuid.uuid4().hex
            rpc(path, "pane_send_keys", pane=pane, keys=token + "\n")
            wait_for(lambda: f"ACK:<{token}>" in screen(path, pane), "fresh VTE input")
            current = next(a for a in host_call("list")["agents"] if a["instance"] == agent["instance"])
            for field in ("instance", "epoch", "pid", "process_start"):
                assert current[field] == agent[field], (field, current, agent)
            assert (root / "starts").read_text().splitlines() == [str(agent["pid"])]

        verify_view()
        state = root / "state/flowmux/state.json"
        wait_for(lambda: state.exists() and surface in state.read_text(), "managed binding persisted")
        before = int((root / "progress").read_text())
        app.kill()
        assert app.wait(timeout=10) == -signal.SIGKILL
        wait_for(lambda: int((root / "progress").read_text()) >= before + 5, "progress after GUI crash")
        # This existing connection must remain usable across the process boundary.
        assert host_call("list")["agents"][0]["state"] == "running"
        app, path = window()
        verify_view()
        print("AGENT_GUI_CRASH_RECONNECT_OK", flush=True)
        # With the GUI gone and every execution stopped, no Host remains. The saved
        # Agent tab must then come back as an ordinary terminal, not a dead view.
        reader.close()
        reader = None
        stream.close()
        stream = None
        app.kill()
        app.wait(timeout=10)
        stopped = subprocess.run([str(cli), "agent", "stop-host"], env=launch_env,
                                 capture_output=True, text=True, timeout=30)
        assert stopped.returncode == 0, (stopped.stdout, stopped.stderr)
        wait_for(lambda: not host_socket.exists(), "stopped Host exits")
        assert agent["instance"] in state.read_text(), "the saved tab still names the execution"
        app, path = window()
        wait_for(lambda: agent["instance"] not in state.read_text(), "saved binding cleared")
        token = "ordinary-" + uuid.uuid4().hex

        def shell_answers():
            # The saved layout appears before its GTK terminal widget is restored.
            if screen(path, pane) is None:
                return False
            rpc(path, "pane_send_keys", pane=pane, keys=f"echo {token}\n")
            time.sleep(.3)
            return (screen(path, pane) or "").count(token) >= 2

        wait_for(shell_answers, "restored tab is an ordinary shell")
        assert any(t["id"] == surface for w in rpc(path, "workspace_tree")["tree"]["workspaces"]
                   for p in w["panes"] for t in p["tabs"]), "the saved tab is kept"
        # The restored terminal's shell runs in a new Host; nothing is relaunched there.
        assert (root / "starts").read_text().splitlines() == [str(agent["pid"])], "no provider relaunch"
        print("AGENT_GUI_HOSTLESS_RESTORE_OK", flush=True)
        print("AGENT_GUI_CRASH_OK", flush=True)
    finally:
        failed = sys.exc_info()[0] is not None
        (root / "release").touch()
        if reader is not None:
            reader.close()
        if stream is not None:
            stream.close()
        for app in children:
            if app.poll() is None:
                app.terminate()
                try:
                    app.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    app.kill()
                    app.wait(timeout=10)
        if host_socket.exists():
            try:
                wait_for(lambda: not host_socket.exists(), "fixture Host cleanup", timeout=20)
            except AssertionError:
                if failed:
                    print(f"Fixture Host cleanup failed; inspect {root}", file=sys.stderr)
                else:
                    raise


if __name__ == "__main__":
    main()
