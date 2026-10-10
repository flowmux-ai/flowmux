#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run and continue bounded Claude/Codex tasks in visible Flowmux panes."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


TERMINAL = {"completed", "blocked", "failed", "timed_out"}
MODEL_ROUTES = {
    "codex": {"simple": ("gpt-6-luna", "high"),
              "standard": ("gpt-6.1-sol", "medium"),
              "complex": ("gpt-6-astra", "high")},
    "claude": {"simple": ("haiku", "low"),
               "standard": ("sonnet", "medium"),
               "complex": ("fable", "high")},
}


def model_route(args):
    model, effort = MODEL_ROUTES[args.agent][args.complexity]
    # An explicit model keeps its provider's effort unless the user sets it too.
    return {"complexity": args.complexity, "model": args.model or model,
            "effort": args.effort or (None if args.model else effort)}


def write_json(path, value):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + "\n")
    temporary.replace(path)


def flowmux(cli, socket, *args):
    result = subprocess.run([cli, *(["--socket", socket] if socket else []), "--json", *args],
                            capture_output=True, text=True, timeout=15)
    if result.returncode:
        raise RuntimeError(f"Flowmux {args[0]} failed: {result.stderr.strip() or result.stdout.strip()}")
    value = json.loads(result.stdout)
    if isinstance(value, dict) and "error" in value:
        raise RuntimeError(str(value["error"]))
    return value


def source_context(args, cli):
    session = os.environ.get("CODEX_THREAD_ID") or os.environ.get("CODEX_SESSION_ID")
    if session:
        # Shared Codex servers inherit the first terminal's environment, not this session's.
        origin = flowmux(cli, args.socket or os.environ.get("FLOWMUX_SOCKET_PATH"),
                         "identify", "--session", session)
        socket, pane = origin["socket"], origin["pane"]
    else:
        socket, pane = os.environ.get("FLOWMUX_SOCKET_PATH"), os.environ.get("FLOWMUX_PANE_ID")
    if socket and args.socket and Path(socket).resolve() != Path(args.socket).resolve():
        raise ValueError("--socket differs from the originating window; do not redirect this task")
    if pane and args.pane and pane.removeprefix("pane:") != args.pane.removeprefix("pane:"):
        raise ValueError("--pane differs from the originating pane; do not redirect this task")
    socket, pane = socket or args.socket, pane or args.pane
    if not socket or not pane:
        raise ValueError("Supply the user's --socket and --pane, or run inside a Flowmux pane")
    return str(Path(socket).resolve(strict=True)), pane.removeprefix("pane:")


def context(args):
    cli = shutil.which(args.cli)
    if cli is None:
        raise ValueError(f"Flowmux CLI not found: {args.cli}")
    socket, pane = source_context(args, cli)
    tree = flowmux(cli, socket, "tree")["tree"]["workspaces"]
    workspace = next((w for w in tree if any(p["id"] == pane for p in w["panes"])), None)
    if workspace is None:
        raise ValueError("Originating pane is no longer in the window")
    return {"socket": socket, "pane": pane, "workspace": workspace["id"],
            "workspace_type": workspace["location"]["type"],
            "mode": "team" if workspace["location"]["type"] == "team" else "inline"}


def start(args):
    origin = context(args)
    if origin["mode"] != "team":
        raise ValueError("Worker creation requires a Team workspace; complete the task in the current agent without delegation")
    cli = shutil.which(args.cli)
    socket, pane = origin["socket"], origin["pane"]
    cwd = args.cwd.resolve(strict=True)
    if not cwd.is_dir():
        raise ValueError("--cwd must be a directory")
    task = args.task_file.read_text().strip()
    if not task or not args.role.strip():
        raise ValueError("Role and task must not be empty")
    executable = shutil.which(args.agent)
    if executable is None:
        raise ValueError(f"{args.agent} is not installed/on PATH")
    state_home = os.environ.get("XDG_STATE_HOME") or (
        os.environ.get("XDG_DATA_HOME") or Path.home() / "Library/Application Support"
        if sys.platform == "darwin" else Path.home() / ".local/state")
    jobs = Path(state_home) / "flowmux/team-jobs"
    jobs.mkdir(mode=0o700, parents=True, exist_ok=True)
    # The saved terminal shell points here, so tmp cleanup must not remove it.
    job = Path(tempfile.mkdtemp(prefix="job-", dir=jobs))
    print(f"Job artifacts: {job}", file=sys.stderr, flush=True)
    token = str(uuid.uuid4())
    (job / "task.txt").write_text(task + "\n")
    manifest = {"agent": args.agent, "executable": executable, "cwd": str(cwd),
                "role": args.role, "allow_edits": args.allow_edits,
                "timeout": args.timeout, "socket": socket, "cli": cli,
                "workspace": origin["workspace"], "source_pane": pane,
                "token": token, "created_at": time.time(), **model_route(args),
                # A resolved Flowmux shim still needs the lead's PATH to find its provider.
                "provider_env": {"PATH": os.environ.get("PATH", os.defpath),
                                 **{key: str(Path(os.environ.get(key) or Path.home() / default).resolve())
                                    for key, default in (("CODEX_HOME", ".codex"), ("CLAUDE_CONFIG_DIR", ".claude"))}},
                "executables": {agent: shutil.which(agent) for agent in MODEL_ROUTES},
                "fallback_on_quota": not args.no_fallback}
    (job / "prompt.txt").write_text(worker_prompt(job, manifest))
    write_json(job / "status.json", {"state": "pending"})
    try:
        write_json(job / "job.json", manifest)
        launcher = job / "launch.sh"
        helper = job / "worker.py"
        shutil.copyfile(Path(__file__).resolve(), helper)
        command = shlex.join([sys.executable, str(helper), "_worker", str(job)])
        exports = "".join(f"export {key}={shlex.quote(value)}\n" for key, value in manifest["provider_env"].items())
        resume_commands = exports
        for agent, path in manifest["executables"].items():
            if path:
                resumed = {**manifest, "agent": agent, "executable": path}
                if agent != manifest["agent"]:
                    resumed["model"], resumed["effort"] = MODEL_ROUTES[agent][args.complexity]
                argv = shlex.join(agent_command(job, resumed, resume=True))
                resume_commands += f'{agent}() {{ {argv} "$@"; }}\n'
        launcher.write_text("#!/bin/sh\n" + exports
                            + "unset CLAUDECODE CODEX_THREAD_ID CODEX_SESSION_ID\n"
                            # The GUI's resume command must retain the worker's CLI and permission bounds.
                            + 'if [ "$1" = "-lic" ]; then\n'
                            + f'  exec /bin/sh -ic {shlex.quote(resume_commands)}"$2"\nfi\n'
                            + 'if [ "$#" -gt 0 ]; then exec /bin/sh "$@"; fi\n'
                            + f"{command}\nexec /bin/sh -i\n")
        launcher.chmod(0o700)
        created = flowmux(cli, socket, "team-spawn", pane, "--cwd", str(cwd),
                          "--role", args.role, "--shell", str(launcher))["surface_created"]
        manifest["pane"] = created["pane"]
        manifest["surface"] = created["id"]
        write_json(job / "job.json", manifest)
    except Exception as error:
        # An IPC timeout can occur after dispatch. Never retry an uncertain launch.
        write_json(job / "launch-error.json", {"error": str(error)})
        raise RuntimeError(f"Launch unconfirmed; inspect {job} and Flowmux tree: {error}") from error
    print(json.dumps({"job": str(job), **manifest}, ensure_ascii=False))
    return 0


def worker_prompt(job, manifest):
    return (f"FLOWMUX_TEAM_JOB:{manifest['token']}\nRole: {manifest['role']}. "
            f"Read the assignment at {job / 'task.txt'}.\n"
            + ("Edit only assigned files. " if manifest["allow_edits"] else "Read-only; do not change project files. ")
            + "Do not delegate, commit, push or send external messages.\n"
            'Return only JSON: {"task_status":"completed|blocked|failed","report":"findings, checks, blockers"}. '
            "Use completed only after required checks; keep the report concise.\n")


def quota_error(code, message):
    """Called only for native provider errors, never ordinary agent/tool text."""
    code = str(code).lower()
    if any(value in code for value in ("auth", "permission", "denied", "forbidden")):
        return False
    if any(value in code for value in ("usagelimitexceeded", "usage_limit_exceeded", "usage_limit_reached",
                                       "insufficient_quota", "credits_exhausted")):
        return True
    return any(value in message.lower().replace("’", "'") for value in (
        "you've hit your session limit", "you've hit your usage limit", "usage limit reached",
        "credit balance is too low", "insufficient credits", "out of credits"))


def agent_command(job, manifest, resume=False):
    prompt = [] if resume else ["--", (job / "prompt.txt").read_text()]
    selection = ["--model", manifest["model"]] if manifest.get("model") else []
    effort = manifest.get("effort")
    if manifest["agent"] == "codex":
        if effort:
            selection += ["-c", f'model_reasoning_effort="{effort}"']
        # A dedicated TUI keeps this worker's tools in its own pane process tree.
        return [manifest["executable"], "--no-daemon", "--sandbox",
                "workspace-write" if manifest["allow_edits"] else "read-only", *selection, *prompt]
    if effort:
        selection += ["--effort", effort]
    allowed = "Read,Glob,Grep" + (",Edit,Write" if manifest["allow_edits"] else "")
    session = [] if resume else ["--session-id", manifest["token"]]
    return [manifest["executable"], *session, *selection,
            "--permission-mode", "dontAsk", "--tools", allowed,
            "--allowedTools", allowed, "--strict-mcp-config",
            "--mcp-config", '{"mcpServers":{}}', *prompt]


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


def transcript_report(path, manifest):
    """Accept only a completed turn following this job's exact user prompt marker."""
    marker = "FLOWMUX_TEAM_JOB:" + manifest["token"]
    matched = False
    answer = None
    session = manifest.get("session_id")
    blocker = None
    offset = manifest.get("transcript_offset", 0)
    with path.open("rb") as stream:
        if os.fstat(stream.fileno()).st_size < offset:
            raise ValueError("Recorded transcript was truncated; inspect the session")
        stream.seek(offset)
        lines = stream.read().decode(errors="replace").splitlines()
    for line in lines:
        try:
            event = json.loads(line)
        except ValueError:
            continue  # The writer may not have flushed the last JSONL record yet.
        payload = event.get("payload", {})
        if event.get("type") == "session_meta":
            session = payload.get("id") or payload.get("session_id")
            if manifest.get("session_id") and session != manifest["session_id"]:
                raise ValueError("Follow-up transcript session changed")
        if manifest["agent"] == "codex":
            item = payload.get("item", {})
            user = ((event.get("type") == "response_item" and payload.get("role") == "user")
                    or (event.get("type") == "event_msg" and item.get("type") == "UserMessage")
                    or (event.get("type") == "event_msg" and payload.get("type") == "user_message"))
            if user and marker in json.dumps(payload, ensure_ascii=False):
                matched = True
            elif user and matched and offset:
                raise ValueError("Another user prompt interrupted the tracked turn")
            elif (matched and event.get("type") == "event_msg"
                  and payload.get("type") in ("error", "task_complete")):
                error = payload if payload["type"] == "error" else payload.get("error")
                if isinstance(error, dict):
                    if error.get("will_retry"):
                        continue
                    text = str(error.get("message") or "Provider error")
                    blocker = "quota" if quota_error(error.get("codex_error_info"), text) else "provider_error"
                    answer = json.dumps({"task_status": "blocked", "report": text})
                else:
                    answer = payload.get("last_agent_message")
                break
        else:
            content = event.get("message", {}).get("content", "")
            user_text = content if isinstance(content, str) else "\n".join(
                b.get("text", "") for b in content if b.get("type") == "text")
            if event.get("type") == "user" and marker in user_text:
                matched = True
                session = event.get("sessionId")
                if manifest.get("session_id") and session != manifest["session_id"]:
                    raise ValueError("Follow-up transcript session changed")
            elif matched and offset and event.get("type") == "user" and user_text:
                raise ValueError("Another user prompt interrupted the tracked turn")
            elif matched and event.get("type") == "assistant":
                message = event.get("message", {})
                text = "\n".join(b.get("text", "") for b in message.get("content", []) if b.get("type") == "text")
                if event.get("isApiErrorMessage"):
                    blocker = "quota" if quota_error(event.get("error"), text) else "provider_error"
                    answer = json.dumps({"task_status": "blocked", "report": text or str(event.get("error", "Provider API error"))})
                    break
                # Claude saves thinking and text as separate records with the same stop reason.
                if message.get("stop_reason") == "end_turn" and text.strip():
                    answer = text
                    break
    return matched, session, answer, blocker


def worker_tab(cli, manifest, allow_unbound=False):
    """Resolve only the recorded live worker, never the focused pane."""
    if not manifest.get("session_id"):
        raise ValueError("Worker receipt has no session identity; no input sent")
    tree = flowmux(cli, manifest["socket"], "tree")["tree"]["workspaces"]
    tab = next((t for w in tree if w["id"] == manifest["workspace"]
                and w["location"]["type"] == "team"
                for p in w["panes"] if p["id"] == manifest["pane"]
                for t in p["tabs"] if t["id"] == manifest["surface"]), None)
    if not tab or tab["kind"] != "terminal":
        raise ValueError("Recorded worker surface is missing; no input sent")
    agent = tab.get("agent") or {}
    if agent.get("name") != manifest["agent"] or (
            agent.get("session_id") != manifest["session_id"]
            and not (allow_unbound and not agent.get("session_id"))):
        raise ValueError("Live worker session does not match the receipt; no input sent")
    return tab


def idle_worker(cli, manifest):
    """Fail closed before terminal input: exact live session, active surface, idle."""
    tab = worker_tab(cli, manifest)
    if not tab["active"]:
        raise ValueError("Recorded worker surface is inactive; no input sent")
    agent = tab["agent"]
    if agent.get("activity") != "idle" or agent.get("status") not in ("idle", "done"):
        raise ValueError("Worker is busy or needs input; inspect its pane before continuing")


def collect_turn(job):
    with (job / "worker.lock").open("a") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        state = json.loads((job / "status.json").read_text())
        if state["state"] in TERMINAL:
            return state
        manifest = json.loads((job / "job.json").read_text())
        try:
            matched, session, answer, blocker = transcript_report(Path(manifest["transcript"]), manifest)
            if matched:
                state.update(state="running", received=True)
            if answer is not None:
                (job / "answer.txt").write_text(answer)
                outcome, report = task_report(answer)
                (job / "result.md").write_text(report)
                state.update(state=outcome, task_status=outcome, session_id=session)
                if blocker:
                    state["blocker_kind"] = blocker
        except (OSError, ValueError) as error:
            state.update(state="failed", error=str(error))
        write_json(job / "status.json", state)
        return state


def followup(args):
    root = args.job
    manifest = json.loads((root / "job.json").read_text())
    if "root_job" in manifest:
        raise ValueError("Use the original worker job, not a turn directory, for followup")
    task = args.task_file.read_text().strip()
    if not task:
        raise ValueError("Task must not be empty")
    cli = shutil.which(args.cli)
    if not cli:
        raise ValueError(f"Flowmux CLI not found: {args.cli}")
    with (root / "followup.lock").open("a") as lock:
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise ValueError("Another followup is being dispatched; do not retry blindly") from error
        origin = context(args)
        if (origin["socket"], origin["workspace"], origin["pane"], origin["mode"]) != (
                manifest["socket"], manifest["workspace"], manifest["source_pane"], "team"):
            raise ValueError("Followup must come from the original lead pane and Team workspace; no input sent")
        state = json.loads((root / "status.json").read_text())
        turns = sorted((root / "turns").glob("[0-9]*"))
        previous = collect_turn(turns[-1]) if turns else state
        if not previous.get("task_status") or previous.get("blocker_kind"):
            raise ValueError("Previous assignment has no completed task report; inspect it before continuing")
        # The effective attempt may have switched providers for quota.
        manifest.update({key: state[key] for key in ("agent", "model", "effort") if key in state})
        manifest.update(session_id=state["session_id"], transcript=state["transcript"])
        idle_worker(cli, manifest)
        turn = root / "turns" / f"{len(turns) + 1:04d}"
        turn.mkdir(parents=True)
        token = str(uuid.uuid4())
        manifest.update(root_job=str(root), token=token, created_at=time.time(),
                        transcript_offset=Path(manifest["transcript"]).stat().st_size)
        (turn / "task.txt").write_text(task + "\n")
        (turn / "prompt.txt").write_text(worker_prompt(turn, manifest))
        write_json(turn / "job.json", manifest)
        receipt = {"job": str(turn), "root_job": str(root),
                   **{key: manifest[key] for key in ("pane", "surface", "workspace", "socket", "session_id")}}
        # Persist uncertainty before any input; status can recover a received reply.
        write_json(turn / "status.json", {"state": "dispatch_unknown", "received": False,
                   **{key: manifest[key] for key in ("agent", "model", "effort", "session_id", "transcript") if key in manifest}})
        try:
            before = flowmux(cli, manifest["socket"], "read-screen", manifest["pane"])["screen_contents"]["text"]
            (turn / "screen-before.txt").write_text(before)
            prompt = f"FLOWMUX_TEAM_JOB:{token} Read the assignment and response contract at {turn / 'prompt.txt'}."
            if any(ord(c) < 32 for c in prompt):
                raise ValueError("Control characters in prompt path")
            idle_worker(cli, manifest)
            flowmux(cli, manifest["socket"], "send-keys", manifest["pane"], prompt.replace("\\", "\\\\"))
            deadline = time.monotonic() + 5
            while True:
                screen = flowmux(cli, manifest["socket"], "read-screen", manifest["pane"])["screen_contents"]["text"]
                (turn / "screen-pasted.txt").write_text(screen)
                if token in "".join(screen.split()):
                    break
                if time.monotonic() >= deadline:
                    raise ValueError("Pasted marker not visible; inspect the pane, Enter was not sent")
                time.sleep(0.1)
            idle_worker(cli, manifest)
            flowmux(cli, manifest["socket"], "send-key", "Enter", "--pane", manifest["pane"])
        except Exception as error:
            write_json(turn / "dispatch-error.json", {"error": str(error)})
            raise RuntimeError(f"Follow-up unconfirmed; inspect {turn}. Do not resend: {error}") from error
        collect_turn(turn)
        print(json.dumps(receipt, ensure_ascii=False))
    return 0


def transcript_candidates(manifest):
    env = {**os.environ, **manifest.get("provider_env", {})}
    if manifest["agent"] == "codex":
        home = Path(env.get("CODEX_HOME", Path.home() / ".codex")) / "sessions"
        candidates = home.glob("*/*/*/*.jsonl")
    else:
        home = Path(env.get("CLAUDE_CONFIG_DIR", Path.home() / ".claude")) / "projects"
        candidates = home.glob("*/" + manifest["token"] + ".jsonl")
    return [p for p in candidates if p.stat().st_mtime >= manifest["created_at"] - 2]


def run_attempt(root, attempt, manifest, env):
    process = None
    state = {"agent": manifest["agent"], "model": manifest.get("model"),
             "effort": manifest.get("effort"), "attempt": attempt.name}
    try:
        process = subprocess.Popen(agent_command(attempt, manifest), cwd=manifest["cwd"], env=env)
        state.update(state="running", pid=process.pid)
        write_json(root / "status.json", state)
        deadline = time.monotonic() + manifest["timeout"]
        bound = None
        while True:
            matches = []
            for path in ([bound] if bound else transcript_candidates(manifest)):
                matched, session, answer, blocker = transcript_report(path, manifest)
                if matched:
                    matches.append((path, session, answer, blocker))
            if len(matches) > 1:
                raise RuntimeError("Job prompt appeared in multiple sessions; refusing an ambiguous result")
            if matches:
                bound, session, answer, blocker = matches[0]
                if state.get("session_id") != session:
                    state.update(session_id=session, transcript=str(bound))
                    write_json(root / "status.json", state)
                if answer is not None:
                    (attempt / "answer.txt").write_text(answer)
                    outcome, report = task_report(answer)
                    (attempt / "result.md").write_text(report)
                    state.update(state=outcome, task_status=outcome,
                                 session_id=session, transcript=str(bound))
                    if blocker:
                        state["blocker_kind"] = blocker
                    break
            if process.poll() is not None:
                raise RuntimeError(f"Agent exited with code {process.returncode} before a completed task report")
            if time.monotonic() >= deadline:
                state.update(state="timed_out", error="No completed report before deadline; inspect the retained interactive pane")
                break
            time.sleep(0.5)
    except Exception as error:
        state.update(state="failed", error=str(error))
        print(f"Flowmux team: {error}", file=sys.stderr, flush=True)
    write_json(attempt / "status.json", state)
    return process, state


def reconcile_stopped_worker(job):
    """Caller holds worker.lock; a supervised job without its owner cannot finish."""
    state = json.loads((job / "status.json").read_text())
    started = job / "started"
    # Older workers did not hold a lock, so absence of a lock cannot prove their death.
    if (state["state"] not in TERMINAL and started.exists()
            and started.read_text() == "lock-v1\n"):
        state.update(state="failed", blocker_kind="interrupted",
                     error="Worker supervisor exited before collecting a final report; inspect the pane and artifacts before starting new work")
        write_json(job / "status.json", state)
    return state


def worker(job):
    with (job / "worker.lock").open("a") as lock:
        while True:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if (job / "started").exists():
                    print("Job already started. No task was repeated.", flush=True)
                    return 1
                time.sleep(0.05)  # A status reader may briefly hold the lock before startup.
        if (job / "started").exists():
            reconcile_stopped_worker(job)
            print(f"Job already started; inspect {job}/status.json. No task was repeated.", flush=True)
            return 1
        (job / "started").write_text("lock-v1\n")
        return run_worker(job)


def run_worker(job):
    process = None
    try:
        deadline = time.monotonic() + 20
        while True:
            manifest = json.loads((job / "job.json").read_text())
            if "pane" in manifest:
                break
            if (job / "launch-error.json").exists() or time.monotonic() >= deadline:
                raise RuntimeError("Worker assignment was not confirmed; agent was not started")
            time.sleep(0.05)
        socket = os.environ.get("FLOWMUX_SOCKET_PATH")
        if (os.environ.get("FLOWMUX_PANE_ID") != manifest["pane"] or not socket
                or str(Path(socket).resolve(strict=True)) != manifest["socket"]):
            raise RuntimeError("Worker pane/socket does not match the assigned job")
        env = {**os.environ, **manifest.get("provider_env", {})}
        for key in ("CLAUDECODE", "CODEX_THREAD_ID", "CODEX_SESSION_ID"):
            env.pop(key, None)  # This is a new independent session, not its lead.
        attempts = []
        for number in (1, 2):
            attempt = job / f"attempt-{number}"
            attempt.mkdir()
            if number == 1:
                shutil.copyfile(job / "prompt.txt", attempt / "prompt.txt")
            else:
                (attempt / "task.txt").write_text(
                    f"Continue the original assignment at {job / 'task.txt'}. "
                    f"The previous provider stopped for quota; its evidence is in {job / 'attempt-1'}. "
                    "Inspect current files and prior progress before acting; do not repeat completed edits. "
                    "If reviewing, report that the provider was substituted.\n")
                (attempt / "prompt.txt").write_text(worker_prompt(attempt, manifest))
            write_json(attempt / "job.json", manifest)
            process, state = run_attempt(job, attempt, manifest, env)
            attempts.append({"path": str(attempt), **state})
            if (number == 1 and manifest.get("fallback_on_quota")
                    and state.get("blocker_kind") == "quota"):
                other = "claude" if manifest["agent"] == "codex" else "codex"
                executable = (manifest["executables"].get(other) if "executables" in manifest
                              else shutil.which(other))
                if executable is None:
                    state["fallback_error"] = f"{other} is not installed/on PATH"
                    break
                # Stop only our own quota-blocked child before handing off; no delayed duplicate run.
                if process.poll() is None:
                    try:
                        process.terminate()
                    except ProcessLookupError:
                        pass
                    try:
                        process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        state["fallback_error"] = "Original worker did not exit; replacement was not started"
                        break
                attempts[-1]["stopped_for_fallback"] = True
                write_json(attempt / "status.json", attempts[-1])
                model, effort = MODEL_ROUTES[other][manifest.get("complexity", "standard")]
                print(f"Flowmux team: {manifest['agent']} quota reached; switching to {other} ({model}) in this pane.", flush=True)
                manifest = {**manifest, "agent": other, "executable": executable,
                            "model": model, "effort": effort, "token": str(uuid.uuid4()),
                            "created_at": time.time(), "fallback_on_quota": False}
                continue
            break
        for name in ("answer.txt", "result.md"):
            if (attempt / name).exists():
                shutil.copyfile(attempt / name, job / name)
        state["attempts"] = attempts
        state["fallback_used"] = len(attempts) > 1
        write_json(job / "status.json", state)
    except Exception as error:
        write_json(job / "status.json", {"state": "failed", "error": str(error)})
        print(f"Flowmux team: {error}", file=sys.stderr, flush=True)
    # Completion is a turn boundary. Keep the final real TUI available for the user.
    if process is not None:
        process.wait()
    return 0


def inspect(job, timeout=0):
    deadline = time.monotonic() + timeout
    manifest = json.loads((job / "job.json").read_text())
    is_turn = "root_job" in manifest
    observation = {}
    while True:
        if is_turn:
            state = collect_turn(job)
        else:
            with (job / "worker.lock").open("a") as lock:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    state = json.loads((job / "status.json").read_text())
                else:
                    state = reconcile_stopped_worker(job)
        observation = {}
        if state["state"] not in TERMINAL and state.get("session_id") and manifest.get("cli"):
            try:
                agent = worker_tab(manifest["cli"], {**manifest, **state}, allow_unbound=True)["agent"]
                if agent.get("activity") == "needs_input" or agent.get("status") == "blocked":
                    observation = {"state": "waiting_input", "input_required": {
                        **{key: manifest[key] for key in ("socket", "workspace", "pane", "surface", "source_pane")},
                        "session_id": state["session_id"], "message": agent.get("message"),
                        "session_verified": agent.get("session_id") == state["session_id"],
                        "action": "Inspect the worker pane and use the provider's normal approval/input UI."}}
            except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
                observation = {"observation_error": str(error)}
        if state["state"] in TERMINAL or "input_required" in observation or time.monotonic() >= deadline:
            break
        time.sleep(min(0.25, max(0, deadline - time.monotonic())))
    report = {"job": str(job), **state, **observation}
    if "input_required" in observation:
        write_json(job / "input-required.json", report)
    if (job / "launch-error.json").exists():
        report["launch_error"] = json.loads((job / "launch-error.json").read_text())
    if (job / "dispatch-error.json").exists():
        report["dispatch_error"] = json.loads((job / "dispatch-error.json").read_text())
    if "task_status" in state:
        report["result"] = (job / "result.md").read_text()
    print(json.dumps(report, ensure_ascii=False, indent=2))
    if state["state"] in TERMINAL:
        return 0 if state["state"] == "completed" else 1
    if "input_required" in observation:
        return 2
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
    route = commands.add_parser("route", help="Preview per-session model selection without starting an agent")
    for command in (launch, route):
        command.add_argument("--agent", choices=("claude", "codex"), required=True)
        command.add_argument("--complexity", choices=("simple", "standard", "complex"), default="standard")
        command.add_argument("--model", help="Explicit provider model; overrides complexity routing")
        command.add_argument("--effort", choices=("low", "medium", "high", "xhigh", "max"))
    launch.add_argument("--role", required=True)
    launch.add_argument("--task-file", type=Path, required=True)
    launch.add_argument("--cwd", type=Path, required=True)
    probe = commands.add_parser("context", help="Resolve the lead and select inline or Team execution")
    for command in (launch, probe):
        command.add_argument("--socket", help="Origin window when launched outside a pane")
        command.add_argument("--pane", help="Origin pane when launched outside a pane")
        command.add_argument("--cli", default=os.environ.get("FLOWMUX_BUNDLED_CLI_PATH", "flowmux"))
    launch.add_argument("--timeout", type=positive, default=300)
    launch.add_argument("--allow-edits", action="store_true")
    launch.add_argument("--no-fallback", action="store_true", help="Do not substitute providers on native quota errors")
    continuation = commands.add_parser("followup", help="Send another tracked assignment to the same idle worker")
    continuation.add_argument("job", type=lambda value: Path(value).resolve())
    continuation.add_argument("--task-file", type=Path, required=True)
    continuation.add_argument("--cli", default=os.environ.get("FLOWMUX_BUNDLED_CLI_PATH", "flowmux"))
    continuation.add_argument("--socket", help="Origin window when launched outside a pane")
    continuation.add_argument("--pane", help="Origin pane when launched outside a pane")
    for name in ("status", "wait", "_worker"):
        command = commands.add_parser(name)
        command.add_argument("job", type=lambda value: Path(value).resolve())
        if name == "wait":
            command.add_argument("--timeout", type=positive, default=60)
    args = parser.parse_args()
    try:
        if args.command == "route":
            print(json.dumps({"agent": args.agent, **model_route(args)}))
            return 0
        if args.command == "context":
            print(json.dumps(context(args)))
            return 0
        if args.command == "start":
            return start(args)
        if args.command == "followup":
            return followup(args)
        if args.command == "_worker":
            return worker(args.job)
        return inspect(args.job, args.timeout if args.command == "wait" else 0)
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"flowmux-team: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
