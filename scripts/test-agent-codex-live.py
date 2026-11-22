#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Opt-in real Codex TUI reconnect check; uses existing login and model access."""

from agent_host_protocol import SOCKET_NAME

import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import time
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("host_fixture", ROOT / "scripts/test-agent-host-gui.py")
host_fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host_fixture)
fixture = host_fixture.fixture


def prompt():
    token = uuid.uuid4().hex
    return (f"Reply with ONLY the concatenation of 'FMRT-', '{token}', and '-한글'. "
            "Do not use tools, inspect files, edit files, or start other agents."), "FMRT-" + token + "-한글"


def process_birth(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
        return None if fields[0] == "Z" else int(fields[19])
    except FileNotFoundError:
        return None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default=str(ROOT / "target/debug/flowmux"))
    parser.add_argument("--cli", default=str(ROOT / "target/debug/flowmuxctl"))
    parser.add_argument("--host", default=str(ROOT / "target/debug/flowmux-daemon"))
    parser.add_argument("--work-seconds", type=int, default=0,
                        help="keep a real Codex tool command running without any GUI for this many seconds (Linux)")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    if args.work_seconds < 0 or (args.work_seconds and sys.platform != "linux"):
        parser.error("--work-seconds requires Linux and a nonnegative duration")
    for name in ("gui", "cli", "host"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    codex = shutil.which("codex")
    assert codex, "Codex CLI must already be installed and logged in"
    h = fixture.Harness(args)
    for variable in ("CODEX_THREAD_ID", "CODEX_SESSION_ID", "CLAUDECODE"):
        h.env.pop(variable, None)
    connection = agent = host = None
    host_socket = h.root / "run" / SOCKET_NAME
    try:
        h.start_display()
        app, gui_socket = h.window("codex-live")
        h.rpc(gui_socket, "workspace_create", root=str(ROOT), name="Real Codex reconnect")
        pane = h.tree(gui_socket)[0]["panes"][0]["id"]
        with (h.root / "host.log").open("w") as log:
            host = h.spawn([args.host, "--agent-host", "--socket", str(host_socket)],
                           stdout=log, stderr=log, cwd=h.root)
        fixture.wait_for(host_socket.exists, "isolated Host")
        connection = host_fixture.HostClient(host_socket, "relay")
        initial, expected = prompt()
        agent = connection.call("launch", launch={
            "request_id": host_fixture.launch_request_id(), "provider": "codex", "label": "Real Codex",
            "argv": [codex, "--no-daemon", "--sandbox", "read-only", "--cd", str(ROOT), initial],
            "cwd": str(ROOT), "env": list(h.env.items()), "size": {"rows": 40, "cols": 140},
        })["agent"]
        assert agent["pid"] not in h.protected_pids
        surface = h.rpc(gui_socket, "agent_view_open", pane=pane,
                        instance=agent["instance"])["surface_created"]["id"]

        def reply(socket, target, text):
            def observed():
                screen = h.screen(socket, target)
                (h.root / "latest-screen.txt").write_text(screen)
                assert "Do you trust the contents" not in screen, "Provider trust decision required; not approved by test"
                return text in screen
            fixture.wait_for(observed, "real model reply (or inspect latest-screen.txt)", timeout=120)

        reply(gui_socket, pane, expected)
        h.pass_check("real Codex model replies through the managed VTE")
        work = None
        if args.work_seconds:
            token = uuid.uuid4().hex
            code = (f"import time; print('FMWORK-' + '{token}', flush=True); "
                    f"time.sleep({args.work_seconds + 30}); print('FMEND-' + '{token}', flush=True)")
            request = ("Run exactly this single read-only command and wait for it to exit: "
                       f"`python3 -u -c {shlex.quote(code)}`. "
                       "Do not edit files, inspect other files, interrupt this command, or start other agents. "
                       f"After it exits, reply with ONLY the concatenation of 'FMDONE-' and '{token}'.")
            h.rpc(gui_socket, "pane_send_keys", pane=pane, keys=request)
            fixture.wait_for(lambda: token in h.screen(gui_socket, pane), "long command prompt displayed")
            h.rpc(gui_socket, "pane_send_keys", pane=pane, keys="\r")

            def command_started():
                scope = Path(f"/proc/{agent['pid']}/cgroup").read_text()
                directory = Path("/sys/fs/cgroup") / scope.strip().removeprefix("0::").lstrip("/")
                for member in (directory / "cgroup.procs").read_text().split():
                    path = Path("/proc") / member
                    try:
                        argv = (path / "cmdline").read_bytes().rstrip(b"\0").split(b"\0")
                        if len(argv) >= 3 and argv[-2:] == [b"-c", code.encode()] and (path / "cgroup").read_text() == scope:
                            pid = int(path.name)
                            if birth := process_birth(pid):
                                return pid, birth
                    except (FileNotFoundError, ProcessLookupError, PermissionError):
                        continue
                return None

            work = fixture.wait_for(command_started, "real provider starts the exact long-running tool command", timeout=120)
        h.rpc(gui_socket, "surface_close", pane=pane, surface=surface)
        h.close_window(app)
        connection.close()
        connection = host_fixture.HostClient(host_socket, "relay")
        current = next(item for item in connection.call("list")["agents"]
                       if item["instance"] == agent["instance"])
        assert current["state"] == "running" and current["attachment"] is None, current
        assert (current["pid"], current["process_start"]) == (agent["pid"], agent["process_start"])
        h.pass_check("tab and last isolated GUI close preserve the actual Codex process")
        if work:
            started = time.monotonic()
            host_birth = process_birth(host.pid)
            observations = h.root / "work-observations.jsonl"
            while True:
                connection.close()
                connection = host_fixture.HostClient(host_socket, "relay")
                current = next(item for item in connection.call("list")["agents"]
                               if item["instance"] == agent["instance"])
                elapsed = time.monotonic() - started
                assert app.poll() is not None and host.poll() is None
                assert process_birth(host.pid) == host_birth
                assert current["state"] == "running" and current["attachment"] is None, current
                assert (current["pid"], current["process_start"]) == (agent["pid"], agent["process_start"])
                assert process_birth(work[0]) == work[1], "provider tool command exited before the headless interval finished"
                h.assert_protected()
                with observations.open("a") as log:
                    log.write(json.dumps(dict(elapsed_seconds=elapsed, host_pid=host.pid, host_birth=host_birth,
                                              agent=current, tool_pid=work[0], tool_birth=work[1])) + "\n")
                print(f"CODEX_HEADLESS_WORK_SECONDS={elapsed:.3f}", flush=True)
                if elapsed >= args.work_seconds:
                    break
                time.sleep(min(20, args.work_seconds - elapsed))
            h.pass_check(f"real Codex and its exact tool command survive {elapsed:.3f}s without any GUI")
        app, gui_socket = h.window("codex-reconnect")
        pane = h.tree(gui_socket)[0]["panes"][0]["id"]
        h.rpc(gui_socket, "agent_view_open", pane=pane, instance=agent["instance"])
        if work:
            reply(gui_socket, pane, "FMDONE-" + token)
            assert process_birth(work[0]) != work[1], "provider replied before its tool command exited"
        else:
            reply(gui_socket, pane, expected)
        followup, fresh = prompt()
        h.rpc(gui_socket, "pane_send_keys", pane=pane, keys=followup)
        fixture.wait_for(lambda: fresh[5:-3] in h.screen(gui_socket, pane), "fresh prompt displayed")
        h.rpc(gui_socket, "pane_send_keys", pane=pane, keys="\r")
        reply(gui_socket, pane, fresh)
        connection.close()
        connection = host_fixture.HostClient(host_socket, "relay")
        current = next(item for item in connection.call("list")["agents"]
                       if item["instance"] == agent["instance"])
        assert (current["pid"], current["process_start"]) == (agent["pid"], agent["process_start"])
        h.pass_check("reconnected same Codex renders previous TUI and answers fresh Hangul round trip")
        connection.call("terminate", instance=agent["instance"], epoch=agent["epoch"], mode="terminate")
        host.wait(timeout=15)
        assert host.returncode == 0 and not host_socket.exists()
        assert not Path(f"/proc/{agent['pid']}").exists()
        h.close_window(app)
        h.assert_protected()
        h.pass_check("explicit termination reaps this provider and empty Host without affecting user GUI")
        print("AGENT_CODEX_LIVE_OK", flush=True)
    finally:
        if agent is not None and host is not None and host.poll() is None:
            try:
                if connection is not None:
                    connection.close()
                connection = host_fixture.HostClient(host_socket, "relay")
                connection.call("terminate", instance=agent["instance"], epoch=agent["epoch"], mode="terminate")
                host.wait(timeout=15)
            except (OSError, AssertionError, subprocess.TimeoutExpired):
                pass
        if connection is not None:
            connection.close()
        h.cleanup()


if __name__ == "__main__":
    main()
