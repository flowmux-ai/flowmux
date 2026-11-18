#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Replay representative native hook payloads through the CLI into an isolated GUI.

Use only a disposable GUI's explicit socket. Creates local test workspaces and
fake agent processes; leaves payload/state evidence in the printed directory.
The existing SSH GUI coverage harness owns GUI startup and normal shutdown.
"""

import argparse
from contextlib import closing
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import tempfile
import time


def focus_gui(socket_path):
    # Workspace selection alone does not make the isolated GUI a foreground app.
    pid = int(Path(socket_path).stem.removeprefix("flowmux-"))
    if sys.platform == "darwin":
        # The native main-thread harness activates AppKit before replay starts.
        return
    else:
        from Xlib import X, display
        with closing(display.Display()) as connection:
            for window in connection.screen().root.query_tree().children:
                owner = window.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)
                if (owner is not None and int(owner.value[0]) == pid
                        and window.get_attributes().map_state == X.IsViewable):
                    window.set_input_focus(X.RevertToParent, X.CurrentTime)
                    connection.sync()
                    assert connection.get_input_focus().focus.id == window.id
                    break
            else:
                raise AssertionError(f"No mapped isolated Flowmux window for PID {pid}")


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
    retry_frames = json.loads((Path(__file__).parent / "fixtures/agent-status/retry-context.json").read_text())
    claude_idle = ("● Response text\n──── Task ─\n❯\u00a0next draft\n────\n"
                   "Fable 5.2 | flowmux\n"
                   "⏵⏵ bypass permissions on (shift+tab to cycle) · ← 2 agents\n")
    source = root / "agent.c"
    source.write_text("""#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    if (argc != 2) return 1;
    FILE *f = fopen(argv[1], "w");
    if (!f) return 2;
    fprintf(f, "%d", getpid()); fclose(f);
    char line[4096];
    while (fgets(line, sizeof(line), stdin)) {
        if (!strncmp(line, "hook ", 5)) {
            system(line + 5);
            continue;
        }
        if (!strncmp(line, "claude-idle", 11)) {
            printf("\\033]2;✳ Task\\007\\033[2J\\033[H"); fputs(CLAUDE_IDLE_FRAME, stdout);
        }
        if (!strncmp(line, "claude-working", 14)) {
            printf("\\033]2;◐ Task\\007\\033[2J\\033[H"); fputs(CLAUDE_WORKING_FRAME, stdout);
        }
        if (!strncmp(line, "screen-working", 14)) {
            printf("\\033[2J\\033[H"); fputs(WORKING_FRAME, stdout);
        }
        if (!strncmp(line, "screen-completed", 16)) {
            printf("\\033]0;\\007\\033[2J\\033[H"); fputs(COMPLETED_FRAME, stdout);
        }
        if (!strncmp(line, "screen-ready", 12)) {
            printf("\\033]0;Ready\\007\\033[2J\\033[H"); fputs("Type your message\\n", stdout);
        }
        if (!strncmp(line, "screen-hook-exit", 16)) {
            printf("\\033]0;⠋ Working...\\007\\033[2J\\033[H");
            fputs("⠋ Executing Hook: flowmux hooks stop\\nType your message\\n", stdout);
        }
        if (!strncmp(line, "screen-gemini-narrow", 20)) {
            printf("\\033[2J\\033[H");
            fputs("⠇ Inspecting the project\\n(esc to cancel, 5s)\\n? for shortcuts\\n* Type your message or @path/to/file\\n", stdout);
        }
        if (!strncmp(line, "screen-approved", 15)) {
            printf("\\033[2J\\033[H"); fputs(APPROVED_FRAME, stdout);
        }
        if (!strncmp(line, "screen-draft", 12)) {
            printf("\\033[2J\\033[H"); fputs(DRAFT_FRAME, stdout);
        }
        if (!strncmp(line, "screen-interrupted", 18)) {
            printf("\\033[2J\\033[H"); fputs(INTERRUPTED_FRAME, stdout);
        }
        if (!strncmp(line, "retry-auto", 10)) {
            printf("\\033[2J\\033[H"); fputs(RETRY_AUTO_FRAME, stdout);
        }
        if (!strncmp(line, "retry-manual", 12)) {
            printf("\\033[2J\\033[H"); fputs(RETRY_MANUAL_FRAME, stdout);
        }
        if (!strncmp(line, "retry-empty", 11)) {
            printf("\\033[2J\\033[H"); fputs(RETRY_EMPTY_FRAME, stdout);
        }
        fflush(stdout);
    }
}
""".replace("CLAUDE_IDLE_FRAME", json.dumps(claude_idle, ensure_ascii=False))
       .replace("CLAUDE_WORKING_FRAME", json.dumps(claude_idle.replace("● Response text", "✻ Forming… (7m 42s · ↓ 33.3k tokens)"), ensure_ascii=False))
       .replace("WORKING_FRAME", json.dumps(frames["working"], ensure_ascii=False))
       .replace("COMPLETED_FRAME", json.dumps(frames["completed"], ensure_ascii=False))
       .replace("APPROVED_FRAME", json.dumps(frames["approved"], ensure_ascii=False))
       .replace("DRAFT_FRAME", json.dumps(frames["draft"], ensure_ascii=False))
       .replace("INTERRUPTED_FRAME", json.dumps(frames["interrupted"], ensure_ascii=False))
       .replace("RETRY_AUTO_FRAME", json.dumps(retry_frames["automatic"], ensure_ascii=False))
       .replace("RETRY_MANUAL_FRAME", json.dumps(retry_frames["manual"], ensure_ascii=False))
       .replace("RETRY_EMPTY_FRAME", json.dumps(retry_frames["empty"], ensure_ascii=False)))

    def rpc(verb, **fields):
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(10)
            stream.connect(args.socket)
            stream.sendall((json.dumps(dict(id=1, kind="request", verb=verb, **fields)) + "\n").encode())
            response = json.loads(stream.makefile().readline())
        assert "error" not in response, response
        return response

    with (root / "events.jsonl").open("w", buffering=1) as evidence:
        for name in ("claude", "codex", "gemini", "antigravity", "cline"):
            executable = root / ("agy" if name == "antigravity" else name)
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
            other_pane = None
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
            assert observed()["status_evidence"]["source"] == "process", observed()

            def hook(event, activity, **fields):
                payload = dict(session_id=f"fixture-{name}", hook_event_name=event,
                               cwd=str(root), future_extension={"ignored": True})
                payload.update(fields)
                expected_session_id = payload["session_id"]
                if name == "cline":
                    # Cline resumes the root session, not the client conversation.
                    expected_session_id = "1700000000000_root"
                    del payload["session_id"]
                    payload["taskId"] = "conv_1700000000001_task"
                    payload.update(agent_id="agent-root", parent_agent_id=None,
                                   sessionContext={"rootSessionId": expected_session_id})
                command = [args.cli, "--socket", args.socket, "hooks", name, event]
                if other_surface:
                    command = ["/bin/sh", "-c", '"$@"; result=$?; exit $result',
                               "--managed-daemon", *command]
                if name == "claude":
                    # Native Claude hooks are children of the agent, not this replay controller.
                    prefix = root / f"hook-{time.time_ns()}"
                    script = prefix.with_suffix(".sh")
                    stdin = prefix.with_suffix(".json")
                    stdout = prefix.with_suffix(".out")
                    stderr = prefix.with_suffix(".err")
                    done = prefix.with_suffix(".exit")
                    stdin.write_text(json.dumps(payload))
                    script.write_text(shlex.join([
                        "env", *(f"{k}={v}" for k, v in hook_env.items() if k.startswith("FLOWMUX_")),
                        *command]) + f" <{shlex.quote(str(stdin))} >{shlex.quote(str(stdout))}"
                        + f" 2>{shlex.quote(str(stderr))}\n"
                        + f"result=$?; printf '%s' \"$result\" >{shlex.quote(str(done))}\n")
                    rpc("pane_send_keys", pane=pane,
                        keys="hook " + shlex.join(["/bin/sh", str(script)]) + "\r")
                    deadline = time.monotonic() + 10
                    while not done.exists() or not done.read_text():
                        assert time.monotonic() < deadline, ("Native hook did not finish", script)
                        time.sleep(.05)
                    assert done.read_text() == "0", (script, stdout.read_text(), stderr.read_text())
                else:
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
                            and value.get("session_id") == expected_session_id):
                        break
                    assert time.monotonic() < deadline, (
                        name, event, payload, activity, value,
                        rpc("pane_read_screen", pane=pane))
                    time.sleep(.05)
                assert value["name"] == name and value["session_id"] == expected_session_id, value
                assert value["source"] == "flowmux:hook", value
                assert value["status_evidence"]["source"] == "hook", value
                statuses = {"running": ("working",), "needs_input": ("blocked",),
                            "idle": ("idle", "done")}
                expected = ("unknown",) if name == "codex" and event == "session-start" else statuses[activity]
                assert value["status"] in expected, value
                if event == "stop" and activity == "idle":
                    outcome, label = {
                        "ERROR": ("failed", "Response failed"),
                        "USER_CANCELED": ("canceled", "Turn interrupted"),
                    }.get(fields.get("terminationReason"), ("completed", "Response completed"))
                    assert value.get("turn_outcome") == outcome, value
                    assert value.get("status_text") == label, value
                elif event == "interrupt" and activity == "idle":
                    assert value.get("turn_outcome") == "canceled", value
                elif event == "stop-failure":
                    assert value.get("turn_outcome") == "failed", value
                elif activity == "running":
                    assert value.get("turn_outcome") is None, value
                if other_surface:
                    other = observed(other_surface)
                    assert other and other["status"] == other_status, other
                    assert other.get("session_id") == other_session_id, other
                evidence.write(json.dumps(dict(agent=name, event=event, payload=payload, observed=value)) + "\n")

            def expect_waits(expected, status_text=None):
                deadline = time.monotonic() + 5
                while True:
                    value = observed()
                    waits = [(w["scope"], w["reason"], w["count"])
                             for w in value.get("pending_waits", [])]
                    if waits == expected and (status_text is None or value.get("status_text") == status_text):
                        return value.get("pending_waits", [])
                    assert time.monotonic() < deadline, (expected, status_text, value)
                    time.sleep(.05)

            try:
                if name in ("gemini", "antigravity", "cline"):
                    if name == "gemini":
                        rpc("pane_send_keys", pane=pane, keys="screen-gemini-narrow\r")
                        deadline = time.monotonic() + 5
                        while observed()["status"] != "working" and time.monotonic() < deadline:
                            time.sleep(.05)
                        assert observed()["status"] == "working", observed()

                    def completions():
                        return [e for e in rpc("notifications_list", unread_only=False)["notifications"]["entries"]
                                if e.get("surface") == surface and e["level"] == "turn_completed"]

                    for visible in (False, True):
                        background = next(w["id"] for w in rpc("workspace_tree")["tree"]["workspaces"]
                                          if w["id"] != workspace)
                        rpc("workspace_focus", workspace=workspace if visible else background)
                        focus_gui(args.socket)
                        hook("turn-start", "running")
                        before = len(completions())
                        # A persistent composer or old response must not complete a native turn.
                        rpc("pane_send_keys", pane=pane, keys="screen-completed\r")
                        time.sleep(1)
                        assert observed()["status"] == "working", observed()
                        assert len(completions()) == before
                        if name == "antigravity":
                            hook("stop", "running", fullyIdle=False)
                        if name == "gemini":
                            hook("notification", "needs_input", notification_type="ToolPermission")
                            hook("running", "running", hook_event_name="AfterTool")
                        if name == "cline":
                            hook("notification", "needs_input", hook_event_name="agent_error",
                                 error={"name": "Error", "message": "Unauthorized"})
                            assert observed().get("turn_outcome") == "failed", observed()
                            hook("turn-start", "running")
                        if name in ("gemini", "antigravity"):
                            rpc("pane_send_keys", pane=pane, keys="screen-hook-exit\r")
                            hook("stop", "running", fullyIdle=True)
                            time.sleep(1)
                            assert observed()["status"] == "working", observed()
                            assert len(completions()) == before
                            # A later hook vetoes the stop and starts more work.
                            hook("turn-start", "running")
                            time.sleep(.7)
                            assert len(completions()) == before
                            hook("stop", "running", fullyIdle=True)
                            rpc("pane_send_keys", pane=pane, keys="screen-ready\r")
                            deadline = time.monotonic() + 6
                            while observed()["status"] == "working":
                                assert time.monotonic() < deadline, observed()
                                time.sleep(.1)
                        else:
                            hook("stop", "idle", fullyIdle=True)
                        expected = before + (0 if visible else 1)
                        deadline = time.monotonic() + 5
                        while len(completions()) < expected and time.monotonic() < deadline:
                            time.sleep(.05)
                        assert len(completions()) == expected, completions()
                        rpc("pane_send_keys", pane=pane, keys="screen-hook-exit\r")
                        time.sleep(.5)
                        assert observed()["status"] in ("idle", "done"), observed()
                        assert observed().get("turn_outcome") == "completed", observed()
                        rpc("pane_send_keys", pane=pane, keys="screen-ready\r")
                        hook("stop", "idle", fullyIdle=True)
                        time.sleep(.3)
                        assert len(completions()) == expected, completions()
                        hook("turn-start", "running")
                        time.sleep(1)
                        assert observed()["status"] == "working", observed()
                    if name == "antigravity":
                        before = len(completions())
                        for reason, outcome in (("ERROR", "failed"), ("USER_CANCELED", "canceled")):
                            hook("turn-start", "running")
                            hook("stop", "idle", fullyIdle=True, terminationReason=reason)
                            assert observed().get("turn_outcome") == outcome, observed()
                            assert len(completions()) == before
                    print(f"PASS: {name} visible/hidden progress, completion, deduplication and next turn", flush=True)
                    continue
                if name == "claude":
                    subprocess.run([args.cli, "--socket", args.socket, "hooks", name, "session-start"],
                                   input=json.dumps(dict(session_id="unrelated-controller", cwd=str(root))),
                                   text=True, env=hook_env, capture_output=True, check=True, timeout=10)
                    assert observed().get("session_id") is None, observed()
                    assert observed()["status_evidence"]["source"] == "process", observed()
                    print("PASS: unrelated Claude hook caller cannot claim the fixture pane", flush=True)
                    hook("session-start", "idle")
                    hook("prompt-submit", "running")
                    for level in ("needs_input", "error"):
                        rejected = rpc("agent_lifecycle_update", pane=pane, surface=surface,
                            agent=name, seq=1, session_id=f"fixture-{name}",
                            lifecycle={"event":"permission_wait_started", "status_text":"Stale wait"},
                            notification={"title":"Rejected lifecycle fixture", "body":level, "level":level})
                        assert rejected["agent_lifecycle_receipt"]["applied"] is False, rejected
                        entries = rpc("notifications_list", unread_only=False)["notifications"]["entries"]
                        assert all(e["title"] != "Rejected lifecycle fixture" for e in entries), entries
                        assert observed()["activity"] == "running"
                    question = dict(tool_name="AskUserQuestion", tool_use_id="a")
                    hook("pre-tool-use", "needs_input", **question)
                    initial_wait = expect_waits([("tool", "question", 1)], "Waiting for answer")
                    assert initial_wait[0]["actor"] == "user" and initial_wait[0]["retry"] == "unknown"
                    hook("pre-tool-use", "needs_input", **question)
                    assert expect_waits([("tool", "question", 1)]) == initial_wait
                    hook("pre-tool-use", "needs_input", tool_name="AskUserQuestion", tool_use_id="b")
                    expect_waits([("tool", "question", 2)], "Waiting for answer")
                    hook("post-tool-use", "needs_input", **question)
                    expect_waits([("tool", "question", 1)], "Waiting for answer")
                    hook("post-tool-use", "needs_input", **question)
                    hook("pre-tool-use", "needs_input", **question)
                    hook("post-tool-use", "running", tool_name="AskUserQuestion", tool_use_id="b")
                    hook("pre-tool-use", "running", **question)
                    hook("post-tool-use", "running", tool_name="AskUserQuestion", tool_use_id="early")
                    hook("pre-tool-use", "running", tool_name="AskUserQuestion", tool_use_id="early")
                    hook("permission-request", "needs_input", tool_name="Bash")
                    expect_waits([("permission", "approval", 1)], "Waiting for approval")
                    hook("post-tool-use", "needs_input", tool_name="Bash", tool_use_id="ordinary")
                    hook("post-tool-batch", "running")
                    hook("stop-failure", "needs_input", error="rate_limit")
                    quota = expect_waits([("session", "quota", 1)], "Usage limit reached")
                    assert quota[0]["actor"] == "unknown" and quota[0]["retry"] == "unknown"
                    for frame, retry, actor, label in (
                        ("retry-auto", "automatic", "external", "Waiting to retry"),
                        ("retry-manual", "manual", "user", "Usage limit reached; input required"),
                    ):
                        before_context = observed()
                        rpc("pane_send_keys", pane=pane, keys=frame+"\r")
                        inferred = expect_waits([("session", "quota", 1)], label)[0]
                        assert inferred["retry"] == retry and inferred["actor"] == actor, inferred
                        assert inferred["context_evidence"]["source"] == "screen", inferred
                        after_context = observed()
                        for key in ("activity", "source", "status_evidence", "seq", "turn_outcome"):
                            assert after_context.get(key) == before_context.get(key), (key, before_context, after_context)
                        assert inferred["since"] == quota[0]["since"]
                        evidence.write(json.dumps(dict(agent=name, event=frame, observed=after_context))+"\n")
                        time.sleep(.15)  # Keep this state visible to the native tooltip observer.
                    rpc("pane_send_keys", pane=pane, keys="retry-empty\r")
                    cleared = expect_waits([("session", "quota", 1)], "Usage limit reached")[0]
                    assert cleared["retry"] == "unknown" and "context_evidence" not in cleared
                    print("PASS: screen retry context preserves native quota wait", flush=True)
                    hook("post-tool-batch", "needs_input")
                    hook("notification", "needs_input", notification_type="quota_auto_resume_stale")
                    manual = expect_waits([("session", "quota", 1)], "Usage limit reached; input required")
                    assert manual[0]["actor"] == "user" and manual[0]["retry"] == "manual"
                    assert manual[0]["since"] == quota[0]["since"]
                    hook("notification", "running", notification_type="quota_auto_resume_fired")
                    expect_waits([])
                    hook("stop-failure", "needs_input", error="rate_limit")
                    hook("notification", "idle", notification_type="quota_auto_resume_disabled")
                    expect_waits([], "Response failed")
                    assert observed()["turn_outcome"] == "failed"
                    for error in ("authentication_failed", "cloud_credential_error"):
                        hook("prompt-submit", "running")
                        hook("stop-failure", "needs_input", error=error)
                        auth = expect_waits([("session", "authentication", 1)], "Authentication required")
                        assert auth[0]["actor"] == "user" and auth[0]["retry"] == "unknown"
                    hook("stop-failure", "needs_input", error="api_error")
                    api = expect_waits([("session", "api_failure", 1)], "API request failed")
                    assert api[0]["actor"] == "unknown" and api[0]["retry"] == "unknown"
                    hook("stop", "idle", last_assistant_message="Completed fixture")
                    expect_waits([])
                    hook("prompt-submit", "running")
                    hook("pre-tool-use", "needs_input", **question)
                    hook("post-tool-use", "running", **question)
                    hook("pre-tool-use", "needs_input", tool_name="ExitPlanMode", tool_use_id="plan")
                    expect_waits([("tool", "approval", 1)], "Waiting for approval")
                    hook("post-tool-use", "running", tool_name="ExitPlanMode", tool_use_id="plan")
                    expect_waits([])
                    print("PASS: typed wait reasons, partial resolution, and scope counts", flush=True)
                    hook("stop", "idle")
                    for visible in (False, True):
                        if visible:
                            rpc("workspace_focus", workspace=workspace)
                        else:
                            away = rpc("workspace_create", name="Other workspace", root=str(root))
                            rpc("workspace_focus", workspace=away["workspace_created"]["id"])
                        hook("prompt-submit", "running")
                        rpc("pane_send_keys", pane=pane, keys="claude-working\r")
                        deadline = time.monotonic() + 5
                        while "Forming" not in (observed().get("status_text") or ""):
                            assert time.monotonic() < deadline, observed()
                            time.sleep(.05)
                        hook("post-tool-use", "running", tool_name="Bash", tool_use_id="delegated")
                        rpc("pane_send_keys", pane=pane, keys="claude-idle\r")
                        deadline = time.monotonic() + 5
                        while observed().get("turn_outcome") != "unknown":
                            assert time.monotonic() < deadline, (observed(), rpc("pane_read_screen", pane=pane))
                            time.sleep(.05)
                        value = observed()
                        assert value["status"] == ("idle" if visible else "done"), value
                        assert value["status_evidence"]["source"] == "screen", value
                        assert value["session_id"] == "fixture-claude", value
                        evidence.write(json.dumps(dict(agent=name, event="claude-delegated-idle", visible=visible, observed=value))+"\n")
                        # The previous idle frame cannot complete a new prompt.
                        hook("prompt-submit", "running")
                        time.sleep(.5)
                        assert observed()["status"] == "working", observed()
                        hook("stop", "idle")
                    print("PASS: Claude delegated completion recovers without Stop, visible and hidden", flush=True)
                else:
                    hook("session-start", "idle")
                    rpc("pane_send_keys", pane=pane, keys="screen-draft\r")
                    deadline = time.monotonic() + 5
                    while observed()["status"] not in ("idle", "done"):
                        assert time.monotonic() < deadline, observed()
                        time.sleep(.05)
                    time.sleep(1)
                    assert observed()["status"] in ("idle", "done"), observed()
                    print("PASS: unsubmitted draft is not an approval request", flush=True)
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
                    rpc("pane_send_keys", pane=pane, keys="screen-interrupted\r")
                    time.sleep(1)
                    assert observed()["status"] in ("idle", "done"), observed()
                    print("PASS: interruption does not reopen transcript requests", flush=True)
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
                        for completion in ("footer", "native", "approved"):
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
                                frame = "screen-approved" if completion == "approved" else "screen-completed"
                                rpc("pane_send_keys", pane=pane, keys=frame + "\r")
                            deadline = time.monotonic() + 5
                            while observed()["status"] not in ("idle", "done"):
                                assert time.monotonic() < deadline, (
                                    observed(), rpc("pane_read_screen", pane=pane))
                                time.sleep(.05)
                            if completion != "native":
                                assert observed()["custom_status"] == "Completed", observed()
                            expected_evidence = "hook" if completion == "native" else "screen"
                            assert observed()["status_evidence"]["source"] == expected_evidence, observed()
                            assert observed()["source"] == "flowmux:hook", observed()
                            if completion == "approved":
                                # Outlast polling so a stale approval phrase cannot reopen the wait.
                                time.sleep(1)
                                assert observed()["status"] in ("idle", "done"), observed()
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
                # Normal EOF avoids reporting fixture cleanup as an external signal death.
                for fixture_pane in (pane, other_pane):
                    if fixture_pane:
                        rpc("pane_send_keys", pane=fixture_pane, keys="\x04")
    print("LIVE_NATIVE_HOOK_MATRIX_OK", flush=True)


if __name__ == "__main__":
    main()
