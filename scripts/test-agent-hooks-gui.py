#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Replay representative native hook payloads through the CLI into an isolated GUI.

Use only a disposable GUI's explicit socket. Creates local test workspaces and
fake agent processes; leaves payload/state evidence in the printed directory.
The existing SSH GUI coverage harness owns GUI startup and normal shutdown.
"""

import argparse
import json
import os
from pathlib import Path
import shlex
import signal
import socket
import subprocess
import sys
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--cli", required=True)
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix="fm-hooks-"))
    print(f"HOOK ARTIFACTS: {root}", flush=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith("FLOWMUX_")}
    # A real executable basename is needed for process identity on both OSes.
    frames = json.loads((Path(__file__).parent / "fixtures/agent-status/codex-goal.json").read_text())
    source = root / "agent.c"
    source.write_text("""#include <stdio.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc != 2) return 1;
    FILE *f = fopen(argv[1], "w");
    if (!f) return 2;
    fprintf(f, "%d", getpid()); fclose(f);
    char line[64];
    while (fgets(line, sizeof(line), stdin)) {
        if (!strncmp(line, "screen-working", 14)) {
            printf("\\033[2J\\033[H"); fputs(WORKING_FRAME, stdout);
        }
        if (!strncmp(line, "screen-completed", 16)) {
            printf("\\033[2J\\033[H"); fputs(COMPLETED_FRAME, stdout);
        }
        fflush(stdout);
    }
}
""".replace("WORKING_FRAME", json.dumps(frames["working"], ensure_ascii=False))
       .replace("COMPLETED_FRAME", json.dumps(frames["completed"], ensure_ascii=False)))

    def rpc(verb, **fields):
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(10)
            stream.connect(args.socket)
            stream.sendall((json.dumps(dict(id=1, kind="request", verb=verb, **fields)) + "\n").encode())
            try:
                response = json.loads(stream.makefile().readline())
            except TimeoutError:
                print(f"IPC_DIAG timeout: {verb}", file=sys.stderr, flush=True)
                if sys.platform == "darwin":
                    sample = root / "timeout-sample.txt"
                    subprocess.run(["/usr/bin/sample", str(os.getppid()), "1", "-file", str(sample)], timeout=15)
                    if sample.exists():
                        print(sample.read_text(), file=sys.stderr, flush=True)
                raise
        assert "error" not in response, response
        return response

    with (root / "events.jsonl").open("w", buffering=1) as evidence:
        for name in ("claude", "codex"):
            executable = root / name
            subprocess.run(["cc", str(source), "-o", str(executable)], check=True, timeout=30)
            created = rpc("workspace_create", name=f"Hook regression {name}", root=str(root))
            workspace = created["workspace_created"]["id"]
            ws = next(w for w in rpc("workspace_tree")["tree"]["workspaces"] if w["id"] == workspace)
            pane = ws["panes"][0]["id"]
            surface = ws["panes"][0]["tabs"][0]["id"]
            pid_file = root / f"{name}.pid"
            rpc("pane_send_keys", pane=pane,
                keys=f"exec {shlex.quote(str(executable))} {shlex.quote(str(pid_file))}\r")
            deadline = time.monotonic() + 10
            while not pid_file.exists() or not pid_file.read_text():
                assert time.monotonic() < deadline, f"{name} fixture did not start"
                time.sleep(.05)
            pid = int(pid_file.read_text())
            hook_env = dict(env, FLOWMUX_PANE_ID=pane, FLOWMUX_SURFACE_ID=surface,
                            FLOWMUX_WORKSPACE_ID=workspace, FLOWMUX_AGENT_PID=str(pid),
                            FLOWMUX_AGENT_NAME=name,
                            FLOWMUX_RUNTIME_DIR=str(Path(args.socket).parent))
            other_pid = None
            other_surface = None
            other_status = "idle"
            other_session_id = None

            def observed(target=surface):
                return next(t for w in rpc("workspace_tree")["tree"]["workspaces"]
                            for p in w["panes"] for t in p["tabs"] if t["id"] == target).get("agent")

            deadline = time.monotonic() + 10
            while not observed():
                assert time.monotonic() < deadline, f"{name} process was not detected"
                time.sleep(.05)

            def hook(event, activity, **fields):
                payload = dict(session_id=f"fixture-{name}", hook_event_name=event,
                               cwd=str(root), future_extension={"ignored": True}, **fields)
                command = [args.cli, "--socket", args.socket, "hooks", name, event]
                if other_surface:
                    command = ["/bin/sh", "-c", '"$@"; result=$?; exit $result',
                               "--managed-daemon", *command]
                subprocess.run(command,
                               input=json.dumps(payload), text=True, env=hook_env,
                               capture_output=True, check=True, timeout=10)
                if event in ("stop", "subagent-stop") and activity == "running":
                    # Outlast the 250ms completion grace; do not accept a
                    # transient running state just before a false completion.
                    time.sleep(.5)
                # Hook delivery is best effort: exit zero alone proves nothing.
                deadline = time.monotonic() + 5
                while True:
                    value = observed()
                    if (value and value["activity"] == activity
                            and value.get("session_id") == payload["session_id"]):
                        break
                    assert time.monotonic() < deadline, (name, event, payload, activity, value)
                    time.sleep(.05)
                assert value["name"] == name and value["session_id"] == payload["session_id"], value
                assert value["source"] == "flowmux:hook", value
                statuses = {"running": ("working",), "needs_input": ("blocked",),
                            "idle": ("idle", "done")}
                expected = ("unknown",) if name == "codex" and event == "session-start" else statuses[activity]
                assert value["status"] in expected, value
                if other_surface:
                    other = observed(other_surface)
                    assert other and other["status"] == other_status, other
                    assert other.get("session_id") == other_session_id, other
                evidence.write(json.dumps(dict(agent=name, event=event, payload=payload, observed=value)) + "\n")

            try:
                if name == "claude":
                    hook("session-start", "idle")
                    hook("prompt-submit", "running")
                    question = dict(tool_name="AskUserQuestion", tool_use_id="a")
                    hook("pre-tool-use", "needs_input", **question)
                    hook("pre-tool-use", "needs_input", **question)
                    hook("pre-tool-use", "needs_input", tool_name="AskUserQuestion", tool_use_id="b")
                    hook("post-tool-use", "needs_input", **question)
                    hook("post-tool-use", "needs_input", **question)
                    hook("pre-tool-use", "needs_input", **question)
                    hook("post-tool-use", "running", tool_name="AskUserQuestion", tool_use_id="b")
                    hook("pre-tool-use", "running", **question)
                    hook("post-tool-use", "running", tool_name="AskUserQuestion", tool_use_id="early")
                    hook("pre-tool-use", "running", tool_name="AskUserQuestion", tool_use_id="early")
                    hook("permission-request", "needs_input", tool_name="Bash")
                    hook("post-tool-use", "needs_input", tool_name="Bash", tool_use_id="ordinary")
                    hook("post-tool-batch", "running")
                    hook("stop-failure", "needs_input", error="rate_limit")
                    hook("post-tool-batch", "needs_input")
                    hook("notification", "running", notification_type="quota_auto_resume_fired")
                    hook("stop", "idle", last_assistant_message="Completed fixture")
                    hook("prompt-submit", "running")
                    hook("pre-tool-use", "needs_input", **question)
                    hook("post-tool-use", "running", **question)
                    hook("stop", "idle")
                else:
                    hook("session-start", "idle")
                    # A shared Codex daemon inherited another, now idle tab's
                    # environment. Every hook must still reach this session.
                    other_root = root / "idle-tab"
                    other_root.mkdir()
                    other_ws = rpc("workspace_create", name="Idle Codex", root=str(other_root))
                    other_ws = next(w for w in rpc("workspace_tree")["tree"]["workspaces"]
                                    if w["id"] == other_ws["workspace_created"]["id"])
                    other_pane = other_ws["panes"][0]["id"]
                    other_surface = other_ws["panes"][0]["tabs"][0]["id"]
                    other_pid_file = root / "idle-codex.pid"
                    rpc("pane_send_keys", pane=other_pane,
                        keys=f"exec {shlex.quote(str(executable))} {shlex.quote(str(other_pid_file))}\r")
                    deadline = time.monotonic() + 10
                    while not other_pid_file.exists() or not other_pid_file.read_text():
                        assert time.monotonic() < deadline, "idle Codex fixture did not start"
                        time.sleep(.05)
                    other_pid = int(other_pid_file.read_text())
                    hook_env.update(FLOWMUX_PANE_ID=other_pane, FLOWMUX_SURFACE_ID=other_surface,
                                    FLOWMUX_WORKSPACE_ID=other_ws["id"], FLOWMUX_AGENT_PID=str(other_pid))
                    # Wait for the independent process scan to establish presence.
                    deadline = time.monotonic() + 10
                    while not any(t.get("agent") for w in rpc("workspace_tree")["tree"]["workspaces"]
                                  for p in w["panes"] for t in p["tabs"] if t["id"] == other_surface):
                        assert time.monotonic() < deadline, "idle Codex was not detected"
                        time.sleep(.05)
                    hook("turn-start", "running", turn_id="root-1")
                    child = dict(agent_id="child", turn_id="child-1")
                    hook("subagent-start", "running", **child)
                    hook("stop", "running", turn_id="root-1")
                    rpc("pane_send_keys", pane=pane, keys="screen-completed\r")
                    time.sleep(.5)
                    assert observed()["status"] == "working", observed()
                    hook("subagent-stop", "idle", **child)
                    hook("turn-start", "running", turn_id="root-2")
                    # A reused child reports a prompt without SubagentStart.
                    hook("turn-start", "running", agent_id="child", turn_id="child-2")
                    hook("subagent-stop", "running", **child)
                    hook("stop", "running", turn_id="root-2")
                    hook("subagent-stop", "idle", agent_id="child", turn_id="child-2")
                    hook("turn-start", "running", turn_id="root-3")
                    # A second pathname to this window is not another candidate.
                    alias = Path(args.socket).with_name("flowmux-99999999.sock")
                    alias.symlink_to(Path(args.socket).name)
                    try:
                        hook("stop", "idle", turn_id="root-3")
                    finally:
                        alias.unlink()
                    print("PASS: socket alias does not suppress completion", flush=True)
                    hook("running", "running", turn_id="root-3")
                    hook("stop", "idle", turn_id="root-3", stop_hook_active=True)
                    hook("turn-start", "running", turn_id="root-4")
                    hook("interrupt", "idle", turn_id="root-4")
                    hook("turn-start", "running", turn_id="root-5")
                    hook("stop", "running", turn_id="root-4")
                    hook("stop", "idle", turn_id="root-5")
                    # Keep the sibling genuinely working while the target
                    # completes. Misrouting must not settle either session.
                    subprocess.run([args.cli, "--socket", args.socket, "hooks", "codex", "turn-start"],
                                   input=json.dumps(dict(session_id="fixture-other-codex",
                                                         cwd=str(other_root), turn_id="other-live")),
                                   text=True, env=hook_env, capture_output=True, check=True, timeout=10)
                    other_status = "working"
                    other_session_id = "fixture-other-codex"
                    for visibility in ("hidden", "focused"):
                        focused = other_ws["id"] if visibility == "hidden" else workspace
                        rpc("workspace_focus", workspace=focused)
                        for completion in ("footer", "native"):
                            turn = f"{visibility}-{completion}"
                            hook("turn-start", "running", turn_id=turn)
                            # An unchanged old footer must not settle a new turn.
                            time.sleep(.3)
                            assert observed()["status"] == "working", observed()
                            rpc("pane_send_keys", pane=pane, keys="screen-working\r")
                            deadline = time.monotonic() + 5
                            while "Working (1s" not in (observed().get("custom_status") or ""):
                                assert time.monotonic() < deadline, observed()
                                time.sleep(.05)
                            if completion == "native":
                                hook("stop", "idle", turn_id=turn)
                                # A late spinner repaint must not reopen a
                                # natively settled turn, visible or hidden.
                                rpc("pane_send_keys", pane=pane, keys="screen-working\r")
                                time.sleep(.5)
                            else:
                                # Recover the final parsed grid without Stop
                                # or any focus change/additional output.
                                rpc("pane_send_keys", pane=pane, keys="screen-completed\r")
                            deadline = time.monotonic() + 5
                            while observed()["status"] not in ("idle", "done"):
                                assert time.monotonic() < deadline, (
                                    observed(), rpc("pane_read_screen", pane=pane))
                                time.sleep(.05)
                            if completion == "footer":
                                assert observed()["custom_status"] == "Completed", observed()
                            hook("turn-start", "running", turn_id=f"after-{turn}")
                            rpc("pane_send_keys", pane=pane, keys="screen-working\r")
                            time.sleep(.3)
                            assert observed()["status"] == "working", observed()
                            assert rpc("workspace_current")["workspace_current"]["id"] == focused
                            print(f"PASS: {visibility} {completion} completion and next turn", flush=True)
                        hook("notification", "needs_input", turn_id=f"after-{turn}", tool_name="Bash")
                        rpc("pane_send_keys", pane=pane, keys="screen-completed\r")
                        time.sleep(.5)
                        assert observed()["status"] == "blocked", observed()
                        hook("running", "needs_input", turn_id=f"after-{turn}", tool_name="Bash")
                        hook("turn-start", "running", turn_id=f"resume-{visibility}")
                        print(f"PASS: {visibility} completion cannot clear permission wait", flush=True)

                    # Corrupt/stale bindings must not let a shared-daemon hook
                    # choose one of two exact matches by window or pane order.
                    rpc("agent_activity_update", pane=other_pane, surface=other_surface,
                        agent="codex", pid=other_pid, source="flowmux:hook",
                        session_id="fixture-codex", seq=time.time_ns(),
                        custom_status="Ready", activity="idle")
                    response = rpc("agent_surface_resolve", agent="codex", session_id="fixture-codex")
                    assert response["agent_surface_ambiguous"]["exact_session"] is True, response
                    rpc("agent_lifecycle_update", pane=other_pane, surface=other_surface,
                        agent="codex", session_id="fixture-codex", seq=time.time_ns(),
                        lifecycle=dict(event="turn_started", turn_id="resume-focused",
                                       status_text="Starting turn"))
                    before = [observed(), observed(other_surface)]
                    assert all(a["status"] == "working" for a in before), before
                    subprocess.run(["/bin/sh", "-c", '"$@"; result=$?; exit $result', "--managed-daemon",
                                    args.cli, "--socket", args.socket, "hooks", "codex", "stop"],
                                   input=json.dumps(dict(session_id="fixture-codex", cwd=str(root),
                                                         turn_id="resume-focused")),
                                   text=True, env=hook_env, capture_output=True, check=True, timeout=10)
                    # A misrouted valid Stop settles only after the grace timer.
                    time.sleep(.5)
                    after = [observed(), observed(other_surface)]
                    assert after == before, (before, after)
                    # Positive control: remove the duplicate binding, then the
                    # same Stop must complete its unique target, not its sibling.
                    other_session_id = "fixture-restored-codex"
                    rpc("agent_activity_update", pane=other_pane, surface=other_surface,
                        agent="codex", pid=other_pid, source="flowmux:hook",
                        session_id=other_session_id, seq=time.time_ns(),
                        custom_status="Ready", activity="idle")
                    rpc("agent_lifecycle_update", pane=other_pane, surface=other_surface,
                        agent="codex", session_id=other_session_id, seq=time.time_ns(),
                        lifecycle=dict(event="turn_started", turn_id="other-restored",
                                       status_text="Starting turn"))
                    hook("stop", "idle", turn_id="resume-focused")
                    print("PASS: ambiguous Stop rejected; identical unique Stop completes", flush=True)
                print(f"PASS: {name} native hook replay", flush=True)
            finally:
                # Only the executable started in this test's new terminal.
                for fixture_pid in (pid, other_pid):
                    if fixture_pid:
                        try:
                            os.kill(fixture_pid, signal.SIGTERM)
                        except ProcessLookupError:
                            pass
    print("LIVE_NATIVE_HOOK_MATRIX_OK", flush=True)


if __name__ == "__main__":
    main()
