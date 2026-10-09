#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Launch a bounded Claude/Codex task in a new Flowmux pane and collect its result."""
import argparse
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time


TERMINAL = {"completed", "blocked", "failed", "timed_out"}


def write_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
    temporary.replace(path)


def flowmux(cli, socket, *args):
    result = subprocess.run([cli, "--socket", socket, "--json", *args],
                            capture_output=True, text=True, timeout=15)
    if result.returncode:
        raise RuntimeError(f"Flowmux {args[0]} failed: {result.stderr.strip() or result.stdout.strip()}")
    value = json.loads(result.stdout)
    if isinstance(value, dict) and "error" in value:
        raise RuntimeError(str(value["error"]))
    return value


def start(args):
    if not args.socket or not args.pane:
        raise ValueError("Supply --socket and --pane, or run inside a Flowmux pane")
    # Pin a stable socket alias before any calls; it can move to another window.
    socket = str(Path(args.socket).resolve(strict=True))
    cwd = args.cwd.resolve(strict=True)
    if not cwd.is_dir():
        raise ValueError("--cwd must be a directory")
    task = args.task_file.read_text().strip()
    if not task or not args.role.strip():
        raise ValueError("Role and task must not be empty")
    executable = shutil.which(args.agent)
    if executable is None:
        raise ValueError(f"{args.agent} is not installed/on PATH")
    cli = shutil.which(args.cli)
    if cli is None:
        raise ValueError(f"Flowmux CLI not found: {args.cli}")
    pane = args.pane.removeprefix("pane:")
    tree = flowmux(cli, socket, "tree")["tree"]["workspaces"]
    workspace = next((w for w in tree if any(p["id"] == pane for p in w["panes"])), None)
    if workspace is None or workspace["location"]["type"] != "local":
        raise ValueError("Source must be a pane in a local workspace")
    job = Path(tempfile.mkdtemp(prefix="flowmux-team-"))
    print(f"Job artifacts: {job}", file=sys.stderr, flush=True)
    prompt = (f"Your role: {args.role}\nWorking directory: {cwd}\n"
              "Complete only the task below. Do not delegate further, commit, push, "
              "or send external messages. Report findings, changed files, checks "
              "and any blockers in your final answer.\n"
              + ("Edits are authorized only within the assigned scope.\n" if args.allow_edits
                 else "This is a read-only task. Do not change project files.\n")
              + f"\nTask:\n{task}\n\n"
              'Return one JSON object, without code fences: '
              '{"task_status":"completed|blocked|failed","report":"your report"}. '
              'Choose exactly one task_status. Use completed only when the assigned '
              'task and required checks are done; blocked for missing access, tools, '
              'input or permissions; failed for unsuccessful work. The nonempty '
              'report must include findings, checks and any blockers.\n')
    (job / "prompt.txt").write_text(prompt)
    manifest = {"agent": args.agent, "executable": executable, "cwd": str(cwd),
                "role": args.role, "allow_edits": args.allow_edits,
                "timeout": args.timeout, "socket": socket,
                "workspace": workspace["id"], "source_pane": pane}
    write_json(job / "status.json", {"state": "pending"})
    try:
        child = flowmux(cli, socket, "split", pane, "--right")["pane_split_done"]["new_pane"]
        manifest["pane"] = child
        write_json(job / "job.json", manifest)
        launcher = job / "launch.sh"
        command = shlex.join([sys.executable, str(Path(__file__).resolve()), "_worker", str(job)])
        launcher.write_text(f"#!/bin/sh\n{command}\nexec /bin/sh -i\n")
        launcher.chmod(0o700)
        flowmux(cli, socket, "focus-pane", child)
        created = flowmux(cli, socket, "new-tab", "--workspace", workspace["id"],
                          "--cwd", str(cwd), "--shell", str(launcher))["surface_created"]
        if created["pane"] != child:
            raise RuntimeError("Focus changed during launch; worker refuses a different pane")
        manifest["surface"] = created["id"]
        write_json(job / "job.json", manifest)
    except Exception as error:
        # An IPC timeout can occur after dispatch. Never retry an uncertain launch.
        write_json(job / "launch-error.json", {"error": str(error)})
        raise RuntimeError(f"Launch unconfirmed; inspect {job} and Flowmux tree: {error}") from error
    print(json.dumps({"job": str(job), **manifest}, ensure_ascii=False))
    return 0


def agent_command(job, manifest):
    if manifest["agent"] == "codex":
        return [manifest["executable"], "exec", "--sandbox",
                "workspace-write" if manifest["allow_edits"] else "read-only",
                "--output-last-message", str(job / "answer.txt"), "-"]
    allowed = "Read,Glob,Grep" + (",Edit,Write" if manifest["allow_edits"] else "")
    return [manifest["executable"], "--print", "--output-format", "json",
            "--permission-mode", "dontAsk", "--tools", allowed,
            "--allowedTools", allowed, "--strict-mcp-config",
            "--mcp-config", '{"mcpServers":{}}']


def task_report(answer):
    try:
        value = json.loads(answer)
    except ValueError as error:
        raise ValueError("Final answer must be a JSON task report; inspect agent logs") from error
    if (not isinstance(value, dict)
            or value.get("task_status") not in ("completed", "blocked", "failed")
            or not isinstance(value.get("report"), str)
            or not value["report"].strip()):
        raise ValueError("Final answer needs task_status (completed/blocked/failed) and a nonempty report")
    return value["task_status"], value["report"]


def worker(job):
    # A restored terminal must never repeat a completed (or interrupted) task.
    try:
        with (job / "started").open("x"):
            pass
    except FileExistsError:
        print(f"Job already started; inspect {job}/status.json. No task was repeated.", flush=True)
        return 1
    state = {"state": "failed"}
    process = None
    def interrupted(_signum, _frame):
        raise KeyboardInterrupt
    previous = {sig: signal.signal(sig, interrupted) for sig in (signal.SIGTERM, signal.SIGHUP)}
    try:
        manifest = json.loads((job / "job.json").read_text())
        worker_socket = os.environ.get("FLOWMUX_SOCKET_PATH")
        if (os.environ.get("FLOWMUX_PANE_ID") != manifest["pane"] or not worker_socket
                or str(Path(worker_socket).resolve(strict=True)) != manifest["socket"]):
            raise RuntimeError("Worker pane/socket does not match the assigned job")
        write_json(job / "status.json", {"state": "running", "pid": os.getpid()})
        print(f"Flowmux team: {manifest['role']} ({manifest['agent']})\nArtifacts: {job}", flush=True)
        env = dict(os.environ)
        # This is an independent pane session, not a nested Claude tool call.
        env.pop("CLAUDECODE", None)
        with (job / "prompt.txt").open() as prompt, (job / "stdout.log").open("w") as output, \
                (job / "stderr.log").open("w") as errors:
            process = subprocess.Popen(agent_command(job, manifest), cwd=manifest["cwd"],
                                       env=env, stdin=prompt, stdout=output, stderr=errors,
                                       start_new_session=True)
            process.wait(timeout=manifest["timeout"])
        if process.returncode:
            raise RuntimeError(f"Agent exited with code {process.returncode}; see stdout.log and stderr.log")
        if manifest["agent"] == "claude":
            response = json.loads((job / "stdout.log").read_text())
            if response.get("is_error") or response.get("permission_denials"):
                raise RuntimeError("Claude reported an error or denied tools; see stdout.log")
            answer = response.get("result", "")
        else:
            answer = (job / "answer.txt").read_text()
        outcome, report = task_report(answer)
        (job / "result.tmp").write_text(report)
        (job / "result.tmp").replace(job / "result.md")
        state = {"state": outcome, "task_status": outcome, "exit_code": 0}
        print(report, flush=True)
    except subprocess.TimeoutExpired:
        state = {"state": "timed_out", "error": "Agent exceeded its execution deadline"}
    except (Exception, KeyboardInterrupt) as error:
        state = {"state": "failed", "error": str(error) or "Interrupted"}
    finally:
        if process is not None:
            # Kill only this worker's private agent group, including timed-out tools.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()
        write_json(job / "status.json", state)
        for sig, handler in previous.items():
            signal.signal(sig, handler)
    print(json.dumps(state), flush=True)
    return 0 if state["state"] == "completed" else 1


def inspect(job, timeout=0):
    deadline = time.monotonic() + timeout
    while True:
        state = json.loads((job / "status.json").read_text())
        if state["state"] in TERMINAL or time.monotonic() >= deadline:
            break
        time.sleep(min(0.25, max(0, deadline - time.monotonic())))
    report = {"job": str(job), **state}
    if (job / "launch-error.json").exists():
        report["launch_error"] = json.loads((job / "launch-error.json").read_text())
    if "task_status" in state:
        report["result"] = (job / "result.md").read_text()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    if state["state"] in TERMINAL:
        return 0 if state["state"] == "completed" else 1
    return 124 if timeout else 0


def positive(value):
    number = int(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("must be a positive integer")
    return number


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    launch = commands.add_parser("start", help="Launch one role/task in a fresh pane")
    launch.add_argument("--agent", choices=("claude", "codex"), required=True)
    launch.add_argument("--role", required=True)
    launch.add_argument("--task-file", type=Path, required=True)
    launch.add_argument("--cwd", type=Path, required=True)
    launch.add_argument("--socket", default=os.environ.get("FLOWMUX_SOCKET_PATH"))
    launch.add_argument("--pane", default=os.environ.get("FLOWMUX_PANE_ID"))
    launch.add_argument("--cli", default=os.environ.get("FLOWMUX_BUNDLED_CLI_PATH", "flowmux"))
    launch.add_argument("--timeout", type=positive, default=300)
    launch.add_argument("--allow-edits", action="store_true")
    for name in ("status", "wait", "_worker"):
        command = commands.add_parser(name)
        command.add_argument("job", type=lambda value: Path(value).resolve())
        if name == "wait":
            command.add_argument("--timeout", type=positive, default=60)
    args = parser.parse_args()
    try:
        if args.command == "start":
            return start(args)
        if args.command == "_worker":
            return worker(args.job)
        return inspect(args.job, args.timeout if args.command == "wait" else 0)
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"flowmux-team: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
