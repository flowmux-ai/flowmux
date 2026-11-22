#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Resume from Sessions, close the tab, and reopen the same Host process in an isolated GUI."""
from agent_host_protocol import SOCKET_NAME

import argparse
import importlib.util
import json
import os
import shutil
import socket as socket_module
from pathlib import Path
import shlex
import sqlite3
import subprocess
import sys
import time
import uuid

from Xlib import X, XK, display
from Xlib.ext import xtest

sys.dont_write_bytecode = True


def module(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


gui = module("gui", "test-ssh-workspace-gui.py")
host_fixture = module("host_fixture", "test-agent-host-gui.py")


def click_target(find, role, name, coordinates):
    previous = None

    def ready():
        nonlocal previous
        node = find(role, name)
        if not node:
            previous = None
            return None
        bounds = node.get_component_iface().get_extents(coordinates)
        rectangle = (bounds.x, bounds.y, bounds.width, bounds.height)
        stable = rectangle == previous and bounds.width > 0 and bounds.height > 0
        previous = rectangle
        return (node, bounds) if stable else None

    # SHOWING can precede popup allocation; use a fresh, settled rectangle.
    return gui.wait_for(ready, f"{name} stable click bounds")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--host", default="target/debug/flowmux-daemon")
    parser.add_argument("--session-flow", action="store_true", help="exercise managed Session Flow fork, resume and Background reuse")
    parser.add_argument("--codex-flow", action="store_true", help="exercise Background reuse and resume of a Codex fork without a startup hook")
    parser.add_argument("--codex-flow-restore", action="store_true", help="focus an already-open Codex fork after GUI restart")
    parser.add_argument("--termination", action="store_true", help="verify explicit interrupt, normal and force termination UI")
    parser.add_argument("--cli-controls", action="store_true", help="verify CLI interrupt, termination and force on live managed execution")
    parser.add_argument("--context", action="store_true", help="query current managed context from actual provider descendants")
    parser.add_argument("--new-agent", action="store_true", help="start via New Agent before checking Resume reuse")
    parser.add_argument("--tab-fallback", action="store_true", help="refuse Host startup and resume in an ordinary tab")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    args.codex_flow = args.codex_flow or args.codex_flow_restore
    args.session_flow = args.session_flow or args.codex_flow
    args.termination = args.termination or args.cli_controls
    for name in ("gui", "cli", "host"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = gui.Harness(args)
    connection = control = keeper = None
    try:
        provider = "codex" if args.codex_flow else "claude"
        display_name = "Codex" if args.codex_flow else "Claude"
        profile = h.root / f"{provider} ' 한글 $(exit 8)"
        history = profile / ("sessions/2026/11/22" if args.codex_flow else "projects/fixture")
        history.mkdir(parents=True)
        project = h.root / "project ' 한글 $(exit 9)"
        project.mkdir()
        session = str(uuid.uuid4())
        child_id = str(uuid.uuid4())

        def transcript(session_id, title, parent=None):
            if args.codex_flow:
                path = history / f"rollout-2026-11-22T00-00-00-{session_id}.jsonl"
                meta = dict(id=session_id, source="cli", cwd=str(project))
                if parent:
                    meta["forked_from_id"] = parent
                records = [dict(type="session_meta", payload=meta),
                           dict(type="response_item", payload=dict(type="message", role="user",
                                content=[dict(type="input_text", text=title)]))]
            else:
                path = history / f"{session_id}.jsonl"
                records = [dict(type="user", sessionId=session_id, cwd=str(project),
                                message=dict(role="user", content=title))]
            path.write_text("".join(json.dumps(record) + "\n" for record in records))
            return path

        transcript(session, "Managed resume fixture")
        shell = h.root / "configured-shell"
        shell.write_text("#!/bin/sh\nexport PATH=" + shlex.quote(str(h.root)) + ":$PATH\n"
                         "export FIXTURE_STARTUP=initialized\n"
                         'if [ "${1-}" = -lic ]; then exec /bin/sh -c "$2"; fi\n'
                         'exec /bin/sh "$@"\n')
        shell.chmod(0o700)
        options = h.root / "config/flowmux/options.json"
        options.parent.mkdir(parents=True, exist_ok=True)
        options.write_text(json.dumps(dict(default_shell=str(shell), auto_resume_agent_sessions=True)))
        h.env.update(GTK_A11Y="atspi", FIXTURE_ROOT=str(h.root), SHELL=str(shell))
        h.env["CODEX_HOME" if args.codex_flow else "CLAUDE_CONFIG_DIR"] = str(profile)
        if args.codex_flow:
            h.env["FIXTURE_ROLLOUT"] = str(history / f"rollout-2026-11-22T00-00-00-{child_id}.jsonl")
            h.env["FIXTURE_CHILD"] = child_id
        if args.termination:
            h.env["FIXTURE_TERMINATION"] = "1"
        if args.context:
            probe = h.root / "context-probe.py"
            probe.write_text('import json, os, pathlib, subprocess, sys\n'
                'root = pathlib.Path(__file__).parent\n'
                'request = json.loads((root / "context-request").read_text())\n'
                'env = dict(os.environ)\n'
                'env.pop("CODEX_THREAD_ID", None)\n'
                'env.pop("CODEX_SESSION_ID", None)\n'
                'env.update(request.get("env", {}))\n'
                'for key in request.get("remove", []): env.pop(key, None)\n'
                f'cli = {args.cli!r}\n'
                f'team = {str(Path(__file__).resolve().parents[1] / ".agents/skills/flowmux-team/scripts/team.py")!r}\n'
                'command = ([sys.executable, team, "context", "--cli", cli] if request.get("team")\n'
                '           else [cli, "--json", *request.get("command", ["identify"]), *request.get("args", [])])\n'
                'result = subprocess.run(command, env=env, text=True, capture_output=True, timeout=10)\n'
                '(root / "context-result.tmp").write_text(json.dumps(dict(code=result.returncode, stdout=result.stdout, stderr=result.stderr)))\n'
                '(root / "context-result.tmp").replace(root / "context-result")\n')
        h.env.pop("AT_SPI_BUS_ADDRESS", None)
        source = h.root / "agent.c"
        source.write_text(r'''#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/select.h>
#include <signal.h>
#include <fcntl.h>
static volatile sig_atomic_t interrupted, stopping;
static void on_interrupt(int sig) { (void)sig; interrupted = 1; }
static void on_stop(int sig) { (void)sig; stopping = 1; }
int main(int argc, char **argv) {
    if (argc > 1 && !strcmp(argv[1], "--help")) {
        puts("--resume --fork-session resume fork"); return 0;
    }
    char path[4096], cwd[4096], line[1024];
    snprintf(path, sizeof path, "%s/starts", getenv("FIXTURE_ROOT"));
    FILE *file = fopen(path, "a"); if (!file) return 1;
    fprintf(file, "%d\n", getpid()); fclose(file);
    snprintf(path, sizeof path, "%s/launch", getenv("FIXTURE_ROOT"));
    file = fopen(path, "w"); if (!file || !getcwd(cwd, sizeof cwd)) return 1;
    const char *profile = getenv(getenv("FIXTURE_ROLLOUT") ? "CODEX_HOME" : "CLAUDE_CONFIG_DIR");
    fprintf(file, "%s\n%s\n%s\n", cwd, profile, getenv("FIXTURE_STARTUP"));
    for (int i = 1; i < argc; ++i) fprintf(file, "%s\n", argv[i]);
    fclose(file);
    if (getenv("FIXTURE_ROLLOUT") && (argc > 1 && !strcmp(argv[1], "fork") ||
        argc > 2 && !strcmp(argv[1], "resume") && !strcmp(argv[2], getenv("FIXTURE_CHILD")))) {
        int rollout = open(getenv("FIXTURE_ROLLOUT"), O_WRONLY | O_CREAT | O_APPEND, 0600);
        if (rollout < 0) return 2;
    }
    printf("fixture-pid:%d\n", getpid()); fflush(stdout);
    if (getenv("FIXTURE_TERMINATION")) {
        signal(SIGINT, on_interrupt); signal(SIGTERM, on_stop);
        snprintf(path, sizeof path, "%s/resist-term", getenv("FIXTURE_ROOT"));
        if (!access(path, F_OK)) {
            pid_t child = fork(); if (child < 0) return 2;
            if (!child) { signal(SIGTERM, SIG_IGN); for (;;) pause(); }
            snprintf(path, sizeof path, "%s/owned-child", getenv("FIXTURE_ROOT"));
            file = fopen(path, "w"); if (!file) return 2;
            fprintf(file, "%d", child); fclose(file);
        }
    }
    unsigned count = 0;
    for (;;) {
        snprintf(path, sizeof path, "%s/context-request", getenv("FIXTURE_ROOT"));
        if (!access(path, F_OK)) {
            if (system("python3 \"$FIXTURE_ROOT/context-probe.py\"")) return 3;
            unlink(path);
        }
        if (interrupted) {
            interrupted = 0;
            snprintf(path, sizeof path, "%s/interrupted", getenv("FIXTURE_ROOT"));
            file = fopen(path, "w"); if (!file) return 2;
            fprintf(file, "%d", getpid()); fclose(file);
        }
        if (stopping) {
            stopping = 0;
            snprintf(path, sizeof path, "%s/resist-term", getenv("FIXTURE_ROOT"));
            if (access(path, F_OK)) return 0;
        }
        snprintf(path, sizeof path, "%s/progress", getenv("FIXTURE_ROOT"));
        file = fopen(path, "w"); if (!file) return 1;
        fprintf(file, "%u", ++count); fclose(file);
        fd_set fds; FD_ZERO(&fds); FD_SET(0, &fds);
        struct timeval timeout = {0, 100000};
        if (select(1, &fds, NULL, NULL, &timeout) > 0) {
            if (!fgets(line, sizeof line, stdin)) return 0;
            if (!strcmp(line, "quit\n")) return 0;
            printf("ACK:%s", line); fflush(stdout);
        }
    }
}
''')
        subprocess.run(["cc", str(source), "-o", str(h.root / provider)], check=True, timeout=30)
        h.start_display()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ.pop("AT_SPI_BUS_ADDRESS", None)
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        host_socket = h.root / "run" / SOCKET_NAME
        if args.cli_controls:
            for verb in ("interrupt", "terminate"):
                absent = subprocess.run([args.cli, "agent", verb, str(uuid.uuid4())],
                    env=h.env, text=True, capture_output=True, timeout=10)
                assert absent.returncode != 0 and not host_socket.exists(), absent.stderr
            h.pass_check("CLI controls fail without a Host and never start one")
        if args.tab_fallback:
            # Refuse the user service manager without the Flatpak side effects of FLATPAK_ID.
            refuse = h.root / "refuse-service"
            refuse.mkdir()
            (refuse / "systemd-run").write_text('#!/bin/sh\necho "$@" >> "$FIXTURE_ROOT/service-refused"\nexit 1\n')
            (refuse / "systemd-run").chmod(0o700)
            h.env["PATH"] = f"{refuse}{os.pathsep}{h.env['PATH']}"
        process, socket = h.window("session-resume")
        workspace = h.rpc(socket, "workspace_create", root=str(h.root), name="Managed resume")["workspace_created"]["id"]
        h.rpc(socket, "workspace_focus", workspace=workspace)
        pane = h.workspace(socket, workspace)["panes"][0]
        pane, original = pane["id"], pane["tabs"][0]["id"]
        connection = display.Display(h.env["DISPLAY"])

        def find(role, name=None, contains=None, *, all_matches=False, showing=True, description=None):
            desktop = Atspi.get_desktop(0)
            desktop.clear_cache()
            pending = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
            pending = [node for node in pending if node and node.get_process_id() == process.pid]
            matches = []
            while pending:
                node = pending.pop()
                if not node:
                    continue
                node.clear_cache()
                if ((role is None or node.get_role() == role) and (name is None or node.get_name() == name)
                        and (contains is None or contains in node.get_name())
                        and (description is None or node.get_description() == description)
                        and (not showing or node.get_state_set().contains(Atspi.StateType.SHOWING))):
                    if not all_matches:
                        return node
                    matches.append(node)
                pending.extend(node.get_child_at_index(i) for i in range(node.get_child_count()))
            return matches if all_matches else None

        def window():
            return next((w for w in connection.screen().root.query_tree().children
                if (owner := w.get_full_property(connection.intern_atom("_NET_WM_PID"), X.AnyPropertyType)) is not None
                and int(owner.value[0]) == process.pid and w.get_attributes().map_state == X.IsViewable), None)

        def click(role, name):
            mapped = gui.wait_for(window, "mapped isolated window")
            mapped.set_input_focus(X.RevertToParent, X.CurrentTime)
            node, bounds = click_target(find, role, name, Atspi.CoordType.WINDOW)
            point = connection.screen().root.translate_coords(mapped, bounds.x + bounds.width // 2, bounds.y + bounds.height // 2)
            h.log("click", name=name, x=point.x, y=point.y,
                  bounds=[bounds.x, bounds.y, bounds.width, bounds.height])
            xtest.fake_input(connection, X.MotionNotify, x=point.x, y=point.y)
            connection.sync()
            time.sleep(.08)
            xtest.fake_input(connection, X.ButtonPress, 1)
            xtest.fake_input(connection, X.ButtonRelease, 1)
            connection.sync()

        def resume():
            click(Atspi.Role.LABEL, "Managed resume fixture")
            gui.wait_for(lambda: (node := find(Atspi.Role.PUSH_BUTTON, "Resume in new tab"))
                         and node.get_state_set().contains(Atspi.StateType.SENSITIVE), "resume enabled")
            click(Atspi.Role.PUSH_BUTTON, "Resume in new tab")

        def live():
            agents = control.call("list")["agents"]
            return [a for a in agents if a["state"] == "running"]

        gui.wait_for(window, "mapped isolated window").configure(width=1600, height=1000)
        connection.sync()
        mapped = gui.wait_for(window, "mapped isolated window")
        mapped.set_input_focus(X.RevertToParent, X.CurrentTime)
        connection.sync()
        # Xvfb has no window manager; AT-SPI ACTIVE is not the keyboard focus receipt.
        gui.wait_for(lambda: getattr(connection.get_input_focus().focus, "id", None) == mapped.id,
                     "native keyboard focus on isolated window")
        # The footer scrolls; its AT-SPI action can open Sessions while the button is clipped.
        try:
            sessions = gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Agent sessions", showing=False),
                                   "sessions button ready")
        except AssertionError:
            try:
                (h.root / "session-resume-startup.json").write_text(json.dumps([
                    dict(name=n.get_name(), role=n.get_role_name(), states=[str(s) for s in n.get_state_set().get_states()])
                    for n in find(None, all_matches=True, showing=False)], indent=2))
                from PIL import ImageGrab
                ImageGrab.grab(xdisplay=h.env["DISPLAY"]).save(h.root / "session-resume-startup.png")
            except Exception as error:
                print(f"Startup diagnostics failed: {error}", file=sys.stderr, flush=True)
            raise
        action = sessions.get_action_iface()
        assert action and action.get_n_actions() and action.do_action(0), "open Agent sessions"
        click(Atspi.Role.TOGGLE_BUTTON, f"{display_name} sessions")
        click(Atspi.Role.CHECK_BOX, "YOLO")
        if args.tab_fallback:
            previous_socket = h.root / "run/flowmux-agent-host.sock"
            with socket_module.socket(socket_module.AF_UNIX) as previous:
                previous.bind(str(previous_socket))
                previous.listen(8)
                resume()
                gui.wait_for(lambda: find(Atspi.Role.LABEL, contains="may already own this session"),
                             "resume blocked while an older Host is running")
                assert not (h.root / "starts").exists()
                assert not (h.root / "service-refused").exists()
            h.pass_check("A live legacy Host blocks an unverified resume without spawning another execution")
            resume()
            status = gui.wait_for(lambda: find(Atspi.Role.LABEL, contains="in an ordinary tab of this window"),
                                  "tab-bound fallback status", timeout=20)
            assert "Managed launch unavailable" in status.get_name(), status.get_name()
            assert (h.root / "service-refused").exists() and not host_socket.exists()
            tab = gui.wait_for(lambda: next((t["id"] for t in h.workspace(socket, workspace)["panes"][0]["tabs"]
                                             if t["id"] != original), None), "ordinary resume tab")
            started = gui.wait_for(lambda: (h.root / "launch").exists() and (h.root / "starts").read_text().split(),
                                   "tab-bound provider start")
            assert (h.root / "launch").read_text().splitlines() == [str(project), str(profile), "initialized",
                                                                   "--resume", session]
            gui.wait_for(lambda: f"fixture-pid:{started[0]}" in h.screen(socket, pane), "tab-bound terminal output")
            h.send(socket, pane, "tab-bound")
            gui.wait_for(lambda: "ACK:tab-bound" in h.screen(socket, pane), "input reaches the tab-bound session")
            assert (h.root / "starts").read_text().split() == started and not host_socket.exists()
            assert any(t["id"] == tab for t in h.workspace(socket, workspace)["panes"][0]["tabs"])
            h.pass_check("Refused Host startup resumes once in an ordinary tab and reports why")
            return
        if args.new_agent:
            def new_agent():
                click(Atspi.Role.PUSH_BUTTON, "Workspace creation options")
                click(Atspi.Role.PUSH_BUTTON, "New Agent…")
            new_agent()
            click(Atspi.Role.PUSH_BUTTON, "Cancel")
            assert not host_socket.exists() and not (h.root / "starts").exists()
            h.pass_check("Cancelling New Agent leaves the layout and Host execution unchanged")
            new_agent()
            field = gui.wait_for(lambda: find(Atspi.Role.TEXT, "Project directory"), "project field")
            assert field.get_editable_text_iface().set_text_contents(str(h.root / "missing"))
            click(Atspi.Role.PUSH_BUTTON, "Start agent")
            gui.wait_for(lambda: find(Atspi.Role.LABEL, "Cannot start agent"), "invalid launch error")
            click(Atspi.Role.PUSH_BUTTON, "OK")
            assert not host_socket.exists() and not (h.root / "starts").exists()
            h.pass_check("New Agent rejects an invalid project before launching a provider")
            new_agent()
            field = gui.wait_for(lambda: find(Atspi.Role.TEXT, "Project directory"), "project field")
            assert field.get_editable_text_iface().set_text_contents(str(project))
            permissions = find(Atspi.Role.CHECK_BOX, "Automatically approve agent actions")
            assert permissions and not permissions.get_state_set().contains(Atspi.StateType.CHECKED)
        # An empty Host retires after 10s; start it only when admission is ready.
        with (h.root / "host.log").open("w") as log:
            host = h.spawn([args.host, "--agent-host", "--socket", str(host_socket)], stdout=log, stderr=log)
        gui.wait_for(host_socket.exists, "isolated Host")
        control = host_fixture.HostClient(host_socket, "relay")
        if args.new_agent:
            click(Atspi.Role.PUSH_BUTTON, "Start agent")
        else:
            resume()
        agent = gui.wait_for(lambda: next(iter(live()), None), "GUI resume launches Host agent")
        pid = agent["pid"]
        birth = agent["process_start"]
        gui.wait_for(lambda: f"fixture-pid:{pid}" in h.screen(socket, pane), "resume terminal output")
        metadata = (h.root / "launch").read_text().splitlines()
        expected = [str(project), str(profile), "initialized"]
        if not args.new_agent:
            expected += ["resume" if args.codex_flow else "--resume", session]
        assert metadata == expected, metadata
        assert agent["provider"] == provider
        h.pass_check(("New Agent" if args.new_agent else "Sessions resume") +
                     " uses Host, configured shell startup, literal project/config and native argv")
        if args.new_agent:
            managed_tab = next(t for t in h.workspace(socket, workspace)["panes"][0]["tabs"] if t["active"])
            local_tab = h.rpc(socket, "surface_create", workspace=workspace, cwd=str(project))["surface_created"]
            codex = h.root / "codex"
            shutil.copyfile(shutil.which("sleep"), codex)
            codex.chmod(0o700)
            h.send(socket, pane, f"exec {shlex.quote(str(codex))} 120")
            h.rpc(socket, "agent_activity_update", pane=pane, surface=local_tab["id"], agent="codex",
                  status="working", source="flowmux:hook", session_id=str(uuid.uuid4()))
            office = gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "AgentOffice", showing=False),
                                  "AgentOffice button")
            assert office.get_action_iface().do_action(0)
            gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, contains="codex · "), "terminal agent in Office")
            gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, contains="Unknown · checking in"),
                         "managed agent in Office before its first hook")
        control.call("native_session", instance=agent["instance"], epoch=agent["epoch"], pid=pid,
                     process_start=birth, session_id=session, seq=1, event=dict(event="started", status="idle"))
        if args.new_agent:
            character = gui.wait_for(lambda: next((n for n in find(Atspi.Role.PUSH_BUTTON,
                contains="Idle · resting", all_matches=True) if n.get_name().startswith("claude · ")), None),
                "managed character updates to idle without reopening Office")
            assert find(Atspi.Role.LABEL, contains="2 teammates · ")
            click(Atspi.Role.PUSH_BUTTON, character.get_name())
            gui.wait_for(lambda: not find(Atspi.Role.PUSH_BUTTON, "Back to workspace"),
                         "managed character closes Office")
            tabs = h.workspace(socket, workspace)["panes"][0]["tabs"]
            assert len(tabs) == 3 and next(t for t in tabs if t["active"])["id"] == managed_tab["id"], tabs
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.rpc(socket, "surface_close", pane=pane, surface=local_tab["id"])
            h.pass_check("AgentOffice counts terminal and managed agents once, updates status, and focuses the original managed tab")
        gui.wait_for(lambda: find(Atspi.Role.LABEL, "Ready · Visible"), "native registry presence")
        if args.new_agent:
            click(Atspi.Role.PUSH_BUTTON, "Options")
            gui.wait_for(lambda: find(Atspi.Role.LABEL, "Background agent idle timeout (not active)"),
                         "honest inactive cleanup label")
            # Locate the saved timeout rather than the display/font spin controls.
            label = find(Atspi.Role.LABEL, "Background agent idle timeout (not active)")
            row = label.get_parent()
            timeout = next(row.get_child_at_index(i) for i in range(row.get_child_count())
                           if row.get_child_at_index(i).get_role() == Atspi.Role.SPIN_BUTTON)
            assert not timeout.get_state_set().contains(Atspi.StateType.SENSITIVE)
            assert timeout.get_value_iface().get_current_value() == 30
            click(Atspi.Role.PUSH_BUTTON, "Close")
            h.pass_check("Options labels automatic cleanup inactive and disables its saved timeout")
            click(Atspi.Role.PUSH_BUTTON, "New Agent")
            click(Atspi.Role.PUSH_BUTTON, "Cancel")
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("The Agents header opens New Agent without changing the running execution on cancel")
        click(Atspi.Role.TOGGLE_BUTTON, f"{display_name} sessions")
        gui.wait_for(lambda: find(Atspi.Role.LABEL, f"{display_name} · 1 sessions · all projects"), "focused managed root history")
        h.pass_check("Focused managed tab browses the provider root's history rather than the relay's descendants")

        def context_probe(expected=True, team=False, **request):
            result_file = h.root / "context-result"
            result_file.unlink(missing_ok=True)
            request["team"] = team
            (h.root / "context-request.tmp").write_text(json.dumps(request))
            (h.root / "context-request.tmp").replace(h.root / "context-request")
            gui.wait_for(lambda: result_file.exists() and not (h.root / "context-request").exists(), "descendant context probe", timeout=15)
            result = json.loads(result_file.read_text())
            assert (result["code"] == 0) is expected, result
            if not expected:
                return result["stderr"]
            if request.get("raw"):
                return result["stdout"].strip()
            identity = json.loads(result["stdout"])
            if request.get("command"):
                return identity
            assert identity["pane"] == pane and identity["workspace"] == workspace, identity
            assert identity["socket"] == str(socket), identity
            if team:
                assert identity["mode"] == "inline", identity
            else:
                current = next(a for a in control.call("list")["agents"] if a["instance"] == agent["instance"])
                assert identity["surface"] == current["view"]["surface"], identity
            return identity

        if args.context:
            stale = {key: str(uuid.uuid4()) for key in ("FLOWMUX_PANE_ID", "FLOWMUX_SURFACE_ID", "FLOWMUX_WORKSPACE_ID", "FLOWMUX_TAB_ID")}
            context_probe(env=stale)
            context_probe(args=["--session", session])
            context_probe(team=True, env=dict(stale, CODEX_THREAD_ID=session))
            assert "does not match" in context_probe(expected=False, args=["--session", "wrong-session"])
            outside = dict(h.env, FLOWMUX_SOCKET_PATH=str(host_socket), FLOWMUX_AGENT_INSTANCE_ID=agent["instance"],
                           FLOWMUX_AGENT_HOST_EPOCH=agent["epoch"])
            result = subprocess.run([args.cli, "--json", "identify"], env=outside, text=True, capture_output=True, timeout=10)
            assert result.returncode and "does not belong" in result.stderr, result
            h.pass_check("Managed identify and Team context ignore stale pane IDs, verify native session and reject copied outside identity")
            for marker in ("FLOWMUX_AGENT_INSTANCE_ID", "FLOWMUX_AGENT_HOST_EPOCH"):
                assert "missing managed context" in context_probe(expected=False, remove=[marker])
                context_probe(expected=False, env={marker: "invalid"})
            context_probe(expected=False, remove=["FLOWMUX_SOCKET_PATH"])
            context_probe(expected=False, env={"FLOWMUX_SOCKET_PATH": str(h.root / "missing-host.sock")})
            context_probe(expected=False, env={"FLOWMUX_SOCKET_PATH": str(socket)})
            h.pass_check("Partial identity, malformed IDs and missing or wrong Host fail without a GUI fallback")
            name = context_probe(command=["session-name"], raw=True, env=stale)
            current_surface = context_probe()["surface"]
            assert name.endswith(f"-{workspace[:4]}-{current_surface[:4]}"), name
            screen = context_probe(command=["read-screen"], env=dict(stale, CODEX_THREAD_ID="stale-inherited-session"))
            context_probe(command=["--socket", str(socket), "read-screen"], env=stale)
            assert "differs from" in context_probe(expected=False, command=["--socket", str(h.root / "wrong-gui.sock"), "read-screen"])
            assert f"fixture-pid:{pid}" in json.dumps(screen), screen
            other_workspace = h.rpc(socket, "workspace_create", root=str(project), name="Context background fixture")["workspace_created"]["id"]
            h.rpc(socket, "workspace_focus", workspace=other_workspace)
            current = context_probe(command=["workspace", "current"], env=stale)
            assert current["workspace_current"]["id"] == workspace, current
            context_probe(command=["read-screen"], env=stale)
            managed_workspace = context_probe(command=["workspace", "new", "--background", "--team",
                "--root", str(project), "--name", "Managed background 한글"], env=stale)["workspace_created"]["id"]
            assert h.rpc(socket, "workspace_current")["workspace_current"]["id"] == other_workspace
            created_workspace = h.workspace(socket, managed_workspace)
            assert created_workspace["name"] == project.name, created_workspace
            assert created_workspace["root"] == str(project), created_workspace
            assert created_workspace["location"] == dict(type="team", root_dir=str(project)), created_workspace
            target_pane = created_workspace["panes"][0]["id"]
            # The new Team workspace is fully materialized before its ACK.
            h.send(socket, target_pane, 'printf \'managed-%s-ready:%s:%s\\n\' background "$PWD" "$FLOWMUX_WORKSPACE_ID"')
            marker = f"managed-background-ready:{project}:{managed_workspace}"
            gui.wait_for(lambda: marker in h.screen(socket, target_pane).replace("\n", "").replace("\r", ""),
                         "new workspace terminal cwd and identity")
            assert h.rpc(socket, "workspace_current")["workspace_current"]["id"] == other_workspace
            context_probe(command=["workspace", "focus", managed_workspace], env=stale)
            assert h.rpc(socket, "workspace_current")["workspace_current"]["id"] == managed_workspace
            assert context_probe(command=["workspace", "current"])["workspace_current"]["id"] == workspace
            context_probe(command=["workspace", "focus", workspace], env=stale)
            assert h.rpc(socket, "workspace_current")["workspace_current"]["id"] == workspace
            h.pass_check("Managed workspace creation is ready on ACK; background creation preserves the visible workspace and explicit focus follows its target")
            foreground = context_probe(command=["workspace", "new",
                "--name", "Managed foreground"], env=stale)["workspace_created"]["id"]
            assert h.workspace(socket, foreground)["location"] == dict(type="local", root_dir=str(project))
            assert h.rpc(socket, "workspace_current")["workspace_current"]["id"] == foreground
            assert context_probe(command=["workspace", "current"])["workspace_current"]["id"] == workspace
            before_tree = h.tree(socket)
            context_probe(expected=False, command=["workspace", "focus", str(uuid.uuid4())])
            error = json.loads(json.loads((h.root / "context-result").read_text())["stdout"])
            assert error["error"]["code"] == "not_found", error
            assert h.tree(socket) == before_tree
            assert h.rpc(socket, "workspace_current")["workspace_current"]["id"] == foreground
            context_probe(command=["workspace", "focus", workspace])
            context_probe()
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("Foreground creation changes the visible workspace while preserving the Agent source and process; unknown focus changes nothing")
            source_surface = context_probe()["surface"]
            before_tree = h.tree(socket)
            opened = context_probe(command=["agent", "open", agent["instance"]], env=stale)["surface_created"]
            assert opened == dict(pane=pane, id=source_surface), opened
            assert h.tree(socket) == before_tree, "self-open duplicated or moved a tab"
            h.pass_check("Managed self-open reuses its verified source tab despite stale inherited pane IDs")
            old_pane, old_workspace = pane, workspace
            opened = context_probe(command=["agent", "open", agent["instance"], "--pane", target_pane], env=stale)["surface_created"]
            assert opened == dict(pane=target_pane, id=source_surface), opened
            pane, workspace = target_pane, managed_workspace
            gui.wait_for(lambda: (next(a for a in live() if a["instance"] == agent["instance"])["view"] or {}).get("pane") == pane,
                         "Host observes managed self-move")
            context_probe()
            h.send(socket, pane, "managed-self-moved")
            gui.wait_for(lambda: "ACK:managed-self-moved" in h.screen(socket, pane), "fresh input after self-move")
            opened = context_probe(command=["agent", "open", agent["instance"], "--pane", old_pane])["surface_created"]
            assert opened == dict(pane=old_pane, id=source_surface), opened
            pane, workspace = old_pane, old_workspace
            gui.wait_for(lambda: (next(a for a in live() if a["instance"] == agent["instance"])["view"] or {}).get("pane") == pane,
                         "Host observes managed return move")
            context_probe()
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            assert next(a for a in live() if a["instance"] == agent["instance"])["process_start"] == birth
            h.pass_check("Managed explicit self-move and return preserve the same surface, PID/birth, single spawn and fresh input")
            before_tree = h.tree(socket)
            for command, code in ((["agent", "open", str(uuid.uuid4())], "invalid_argument"),
                                  (["agent", "open", agent["instance"], "--pane", str(uuid.uuid4())], "not_found")):
                context_probe(expected=False, command=command)
                error = json.loads(json.loads((h.root / "context-result").read_text())["stdout"])
                assert error["error"]["code"] == code, error
                assert h.tree(socket) == before_tree, "unknown Agent or pane changed layout"
            h.pass_check("Managed open rejects an unknown execution or explicit pane without changing layout")
            target = control.call("launch", launch=dict(
                request_id=host_fixture.launch_request_id(), provider="fixture", label="Managed open target",
                argv=[sys.executable, "-u", "-c",
                      "import sys\nprint('managed-target-ready', flush=True)\n"
                      "for line in sys.stdin:\n"
                      " if line.strip() == 'quit': break\n"
                      " print('TARGET-ACK:' + line.strip(), flush=True)\n"],
                cwd=str(project), env=list(h.env.items()), size=dict(rows=24, cols=80)))["agent"]
            h.log("managed_target", pid=target["pid"], process_start=target["process_start"])
            opened = context_probe(command=["agent", "open", target["instance"]], env=stale)["surface_created"]
            assert opened["pane"] == pane, opened
            gui.wait_for(lambda: "managed-target-ready" in h.screen(socket, pane), "managed implicit target attachment")
            before_tree = h.tree(socket)
            assert context_probe(command=["agent", "open", target["instance"]])["surface_created"] == opened
            assert h.tree(socket) == before_tree, "reopening another Agent duplicated a tab"
            assert context_probe()["surface"] == source_surface, "opening another Agent redirected the caller identity"
            h.rpc(socket, "surface_close", pane=pane, surface=opened["id"])
            gui.wait_for(lambda: next(a for a in live() if a["instance"] == target["instance"])["attachment"] is None,
                         "target detaches before explicit reconnect")
            reopened = context_probe(command=["agent", "open", target["instance"], "--pane", target_pane])["surface_created"]
            assert reopened["pane"] == target_pane, reopened
            gui.wait_for(lambda: "managed-target-ready" in h.screen(socket, target_pane), "managed explicit target reconnect")
            h.send(socket, target_pane, "reconnected-target")
            gui.wait_for(lambda: "TARGET-ACK:reconnected-target" in h.screen(socket, target_pane), "reconnected target fresh input")
            observed = next(a for a in live() if a["instance"] == target["instance"])
            assert (observed["pid"], observed["process_start"]) == (target["pid"], target["process_start"])
            context_probe()
            h.send(socket, target_pane, "quit")
            gui.wait_for(lambda: not Path(f"/proc/{target['pid']}").exists(), "managed target exits and is reaped")
            # The finished agent's tab stays as an ordinary terminal; close it like any tab.
            h.rpc(socket, "surface_close", pane=target_pane, surface=reopened["id"])
            h.rpc(socket, "surface_focus", pane=pane, surface=source_surface)
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("Managed open attaches another Background execution to the verified or explicit pane, deduplicates, and reconnects with the same PID/birth and fresh input")
            opened = context_probe(command=["browser", "open", "about:blank"], env=stale)
            browser_pane = opened["browser_pane_opened"]["pane"]
            assert browser_pane != pane, opened
            assert any(p["id"] == browser_pane for p in h.workspace(socket, workspace)["panes"])
            context_probe(command=["browser", "wait", browser_pane, "--ready-state", "complete"])
            browser_surface = next(p["tabs"][0]["id"] for p in h.workspace(socket, workspace)["panes"] if p["id"] == browser_pane)
            context_probe(command=["browser", "--surface", browser_surface, "--receipt", "eval", browser_pane,
                                   "document.body.dataset.managed = 'single'; 'single'"])
            batch = [["eval", "document.body.dataset.managed = 'batch'; 'batch'"], ["url"]]
            context_probe(command=["browser", "--surface", browser_surface, "run", browser_pane, json.dumps(batch)])
            value = h.rpc(socket, "browser_eval", pane=browser_pane, source="document.body.dataset.managed")
            assert "batch" in json.dumps(value), value
            h.rpc(socket, "pane_close", pane=browser_pane)
            created = context_probe(command=["new-tab"], env=stale)["surface_created"]
            h.rpc(socket, "surface_close", pane=created["pane"], surface=created["id"])
            h.rpc(socket, "surface_focus", pane=pane, surface=context_probe()["surface"])
            context_probe(command=["notify", "--title", "Managed context", "Explicit notification"], env=stale)
            assert "does not support managed context" in context_probe(expected=False,
                command=["notifications", "list"])
            assert h.rpc(socket, "workspace_current")["workspace_current"]["id"] == workspace
            assert (h.root / "starts").read_text().splitlines() == [str(pid)]
            h.pass_check("Managed descendants route implicit read-screen, workspace current, browser open and new-tab through the Host despite stale environment")

        if args.session_flow:
            database = h.root / "data/flowmux/session-flow.sqlite"

            def rows(query):
                with sqlite3.connect(f"file:{database}?mode=ro", uri=True) as db:
                    return db.execute(query).fetchall()

            def keypress(name, modifier=None):
                code = connection.keysym_to_keycode(XK.string_to_keysym(name))
                if modifier:
                    modifier = connection.keysym_to_keycode(XK.string_to_keysym(modifier))
                    xtest.fake_input(connection, X.KeyPress, modifier)
                xtest.fake_input(connection, X.KeyPress, code)
                xtest.fake_input(connection, X.KeyRelease, code)
                if modifier:
                    xtest.fake_input(connection, X.KeyRelease, modifier)
                connection.sync()

            def fork_menu():
                mapped = gui.wait_for(window, "mapped window")
                bounds = mapped.get_geometry()
                point = connection.screen().root.translate_coords(mapped, bounds.width * 3 // 4,
                                                                  bounds.height // 2)
                mapped.set_input_focus(X.RevertToParent, X.CurrentTime)
                xtest.fake_input(connection, X.MotionNotify, x=point.x, y=point.y)
                connection.sync()
                time.sleep(.08)
                xtest.fake_input(connection, X.ButtonPress, 3)
                xtest.fake_input(connection, X.ButtonRelease, 3)
                connection.sync()
                return gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fork session"), "Fork menu")

            def fork_enabled():
                enabled = fork_menu().get_state_set().contains(Atspi.StateType.SENSITIVE)
                keypress("Escape")
                gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fork session") is None, "closed Fork menu")
                return enabled

            parent_surface = next(t["id"] for t in h.workspace(socket, workspace)["panes"][0]["tabs"] if t["id"] != original)
            click(Atspi.Role.PUSH_BUTTON, "Close sessions")
            gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Close sessions") is None, "closed Sessions panel")
            gui.wait_for(fork_enabled, "managed session fork eligibility", timeout=15)
            assert find(Atspi.Role.PUSH_BUTTON, "Fit") is None
            h.pass_check("Managed Host identity and idle presence enable Fork while Session Flow is hidden")
            if args.codex_flow:
                # Reproduce a previous window leaving a Resume pending for a session that is already open.
                request = str(uuid.uuid4())
                with sqlite3.connect(database) as db:
                    source, = db.execute("SELECT key FROM sessions WHERE json_extract(key, '$.id') = ?",
                                         (session,)).fetchone()
                    intent = dict(request=request, owner=str(uuid.uuid4()), owner_pid=2**31 - 1,
                                  source=json.loads(source), kind="Resume", target_surface=str(uuid.uuid4()))
                    db.execute("INSERT INTO operations(request,source,intent,state) VALUES(?,?,?,'pending')",
                               (request, source, json.dumps(intent)))
            assert fork_menu().get_action_iface().do_action(0)

            def fork_child():
                error = find(None, contains="Cannot reserve this session")
                assert error is None, error.get_name() if error else ""
                return next((a for a in live() if a["instance"] != agent["instance"]), None)

            child = gui.wait_for(fork_child, "managed fork child")
            if args.codex_flow:
                with sqlite3.connect(database) as db:
                    assert db.execute("SELECT state FROM operations WHERE request=?", (request,)).fetchone() == ("pending",)
                    db.execute("DELETE FROM operations WHERE request=?", (request,))
                h.pass_check("An earlier pending Resume does not block the live Codex session from forking")
            panes = gui.wait_for(lambda: panes if len(panes := h.workspace(socket, workspace)["panes"]) == 2 else None,
                                "reserved right split")
            child_pane = next(p for p in panes if p["id"] != pane)
            child_surface = child_pane["tabs"][0]["id"]
            child_pane = child_pane["id"]
            gui.wait_for(lambda: f"fixture-pid:{child['pid']}" in h.screen(socket, child_pane), "fork relay output")
            fork_argv = (expected[:3] + ["fork", session, "--dangerously-bypass-approvals-and-sandbox"]
                         if args.codex_flow else expected + ["--fork-session", "--allow-dangerously-skip-permissions",
                                                            "--permission-mode", "bypassPermissions"])
            assert (h.root / "launch").read_text().splitlines() == fork_argv
            assert len((h.root / "starts").read_text().splitlines()) == 2
            assert len(rows("SELECT request FROM operations WHERE state='pending'")) == 1
            h.pass_check("Fork launches once in Host with the main-branch permission policy and reserved split surface")
            if not args.codex_flow:
                control.call("native_session", instance=child["instance"], epoch=child["epoch"], pid=child["pid"],
                             process_start=child["process_start"], session_id=child_id, seq=1,
                             event=dict(event="started", status="idle"))
                gui.wait_for(lambda: rows("SELECT child FROM forks"), "observed fork lineage")
                assert len(rows("SELECT request FROM operations WHERE state='complete'")) == 1
                assert not rows("SELECT request FROM operations WHERE state='pending'")
                child_live = [json.loads(binding) for key, binding in rows("SELECT key,binding FROM live_sessions")
                              if json.loads(key)["id"] == child_id]
                assert child_live and child_live[0]["pid"] == child["pid"]
                assert child_live[0]["surface"] == child_surface
                assert not (history / f"{child_id}.jsonl").exists()
                h.pass_check("Host root identity completes the reserved fork and publishes lineage before Claude writes history")
            transcript(child_id, "Managed flow child", session)
            click(Atspi.Role.PUSH_BUTTON, "Session Flow")
            gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fit"), "visible Flow graph")
            click(Atspi.Role.PUSH_BUTTON, "↻")
            gui.wait_for(lambda: any(child_id in key for key, in rows("SELECT key FROM sessions WHERE metadata IS NOT NULL")),
                         "saved child history")

            def open_child(attached=False):
                def canvas():
                    return find(None, description="Session Flow graph. Arrow keys select a session; Enter opens it.")

                title = f"{provider} · Managed flow child · "
                suffix = "Open session · select to focus" if attached else "Select to resume in a right split"
                last_child_selection = None
                try:
                    mapped = gui.wait_for(window, "mapped window")
                    mapped.set_input_focus(X.RevertToParent, X.CurrentTime)
                    connection.sync()
                    gui.wait_for(canvas, "accessible Flow canvas")
                    click(Atspi.Role.PUSH_BUTTON, "Fit")
                    gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fit").get_state_set().contains(Atspi.StateType.FOCUSED),
                                 "Flow toolbar keyboard focus")
                    for description in ("Refresh native session history", "Close Session Flow",
                                        "Session Flow graph. Arrow keys select a session; Enter opens it."):
                        keypress("Tab")
                        gui.wait_for(lambda: (n := find(None, description=description))
                                     and n.get_state_set().contains(Atspi.StateType.FOCUSED),
                                     "keyboard focus: " + description)
                    def select_next():
                        nonlocal last_child_selection
                        previous = canvas().get_name()
                        keypress("Down")
                        selected = gui.wait_for(lambda: (n := canvas()) and n.get_name() != previous and n,
                                                "Flow keyboard selection").get_name()
                        h.log("flow-selection", selected=selected)
                        if selected.startswith(title):
                            last_child_selection = selected
                        return selected == title + suffix

                    gui.wait_for(select_next, "child node state: " + suffix, timeout=40)
                    if attached:
                        from PIL import ImageGrab
                        ImageGrab.grab(xdisplay=h.env["DISPLAY"]).save(h.root / "restored-fork.png")
                    keypress("Return")
                except AssertionError as error:
                    try:
                        (h.root / "session-flow-selection.json").write_text(json.dumps([
                            dict(name=n.get_name(), role=n.get_role_name(), description=n.get_description(),
                                 states=[str(s) for s in n.get_state_set().get_states()])
                            for n in find(None, all_matches=True, showing=False)], indent=2))
                        from PIL import ImageGrab
                        ImageGrab.grab(xdisplay=h.env["DISPLAY"]).save(h.root / "session-flow-selection.png")
                    except Exception as error:
                        print(f"Flow selection diagnostics failed: {error}", file=sys.stderr, flush=True)
                    raise AssertionError(f"Expected {title + suffix!r}, last child selection: "
                                         f"{last_child_selection!r}") from error

            def tab_layout():
                return {p["id"]: [t["id"] for t in p["tabs"]]
                        for p in h.workspace(socket, workspace)["panes"]}

            if args.codex_flow_restore:
                h.rpc(socket, "surface_focus", pane=child_pane, surface=child_surface)
                sibling = h.rpc(socket, "surface_create", workspace=workspace, cwd=str(project))["surface_created"]["id"]
                h.wait_active(socket, child_pane, sibling)
                before = tab_layout()
                h.close_window(process)
                assert all(Path(f"/proc/{a['pid']}").exists() for a in (agent, child))
                process, socket = h.window("session-flow-fork-hidden")
                gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Fit"), "saved Session Flow is open")
                h.wait_active(socket, child_pane, sibling)
                control.close()
                control = host_fixture.HostClient(host_socket, "relay")
                restored = next(a for a in live() if a["instance"] == child["instance"])
                assert (restored["pid"], restored["process_start"]) == (child["pid"], child["process_start"])
                assert not (restored.get("presence") or {}).get("session_id")
                open_child(attached=True)
                gui.wait_for(lambda: tab_layout() != before or any(
                    tab["id"] == child_surface and tab["active"]
                    for item in h.workspace(socket, workspace)["panes"] for tab in item["tabs"]),
                             "focus an existing fork or open another tab")
                assert tab_layout() == before, (before, tab_layout())
                gui.wait_for(lambda: f"fixture-pid:{child['pid']}" in h.screen(socket, child_pane),
                             "restored Codex process in the focused tab")
                h.send(socket, child_pane, "flow-restored-fork")
                gui.wait_for(lambda: "ACK:flow-restored-fork" in h.screen(socket, child_pane), "restored fork accepts input")
                assert len((h.root / "starts").read_text().splitlines()) == 2
                assert len(rows("SELECT request FROM operations WHERE state='complete'")) == 1
                h.pass_check("Session Flow focuses the restored Codex fork without a startup hook")
                h.close_window(process)
                return

            if args.codex_flow:
                h.rpc(socket, "surface_focus", pane=child_pane, surface=child_surface)
                keypress("w", "Alt_L")
                click(Atspi.Role.PUSH_BUTTON, "Keep running")
            else:
                h.rpc(socket, "surface_close", pane=child_pane, surface=child_surface)
            gui.wait_for(lambda: child_pane not in tab_layout(), "fork view closed")

            detached = gui.wait_for(lambda: next((a for a in live() if a["instance"] == child["instance"]
                                                  and not a["attachment"]), None),
                                    "same fork execution goes to Background")
            if args.codex_flow:
                assert not (detached.get("presence") or {}).get("session_id"), detached
            h.rpc(socket, "surface_focus", pane=pane, surface=parent_surface)
            before = tab_layout()
            open_child()

            def background_opened():
                error = find(None, contains="Cannot reserve") or find(None, contains="ownership is still updating")
                assert error is None, error.get_name() if error else ""
                after = tab_layout()
                assert after.keys() == before.keys(), ("Background opened a new split", before, after)
                return len(after[pane]) == len(before[pane]) + 1

            gui.wait_for(background_opened, "Flow Background tab opens")
            reopened = gui.wait_for(lambda: next((a for a in live() if a["instance"] == child["instance"]
                                                 and (a.get("attachment") or {}).get("ready")), None),
                                    "same fork execution reconnects")
            assert (reopened["pid"], reopened["process_start"]) == (child["pid"], child["process_start"])
            h.send(socket, pane, "flow-background")
            gui.wait_for(lambda: "ACK:flow-background" in h.screen(socket, pane), "same fork process accepts input")
            assert len((h.root / "starts").read_text().splitlines()) == 2
            assert len(rows("SELECT request FROM operations")) == 1
            h.pass_check("Opening a Background node in Session Flow reconnects the same process without resume or spawn")
            h.send(socket, pane, "quit")
            gui.wait_for(lambda: all(a["instance"] != child["instance"] for a in live()), "child natural exit")
            h.rpc(socket, "surface_focus", pane=pane, surface=parent_surface)
            open_child()
            resumed = gui.wait_for(lambda: next((a for a in live() if a["instance"] != agent["instance"]), None),
                                   "Flow native resume via Host")
            panes = gui.wait_for(lambda: panes if len(panes := h.workspace(socket, workspace)["panes"]) == len(before) + 1 else None,
                                "resumed right split")
            resumed_pane = next(p["id"] for p in panes if p["id"] not in before)
            gui.wait_for(lambda: f"fixture-pid:{resumed['pid']}" in h.screen(socket, resumed_pane), "resumed relay")
            assert (h.root / "launch").read_text().splitlines() == expected[:3] + [
                "resume" if args.codex_flow else "--resume", child_id]
            if not args.codex_flow:
                control.call("native_session", instance=resumed["instance"], epoch=resumed["epoch"], pid=resumed["pid"],
                             process_start=resumed["process_start"], session_id=child_id, seq=1,
                             event=dict(event="started", status="idle"))
            gui.wait_for(lambda: len(rows("SELECT request FROM operations WHERE state='complete'")) == 2,
                         "resume reservation completes")
            assert len((h.root / "starts").read_text().splitlines()) == 3
            h.pass_check("An ended Flow node resumes through Host in a reserved split with one new process")
            h.close_window(process)
            assert all(Path(f"/proc/{a['pid']}").exists() for a in (agent, resumed))
            process, socket = h.window("session-flow-reopened")
            gui.wait_for(lambda: f"fixture-pid:{resumed['pid']}" in h.screen(socket, resumed_pane), "saved Flow relay restored")
            h.send(socket, resumed_pane, "flow-restored")
            gui.wait_for(lambda: "ACK:flow-restored" in h.screen(socket, resumed_pane), "Flow restored input")
            assert len((h.root / "starts").read_text().splitlines()) == 3
            assert next(a for a in live() if a["instance"] == resumed["instance"])["process_start"] == resumed["process_start"]
            h.pass_check("GUI exit restores the Host-backed Flow tab with the same PID/birth and fresh input")
            h.send(socket, resumed_pane, "quit")
            h.rpc(socket, "surface_focus", pane=pane, surface=parent_surface)
            h.send(socket, pane, "quit")
            control.close()
            control = None
            gui.wait_for(lambda: host.poll() == 0, "Flow roots exit and Host retires", timeout=15)
            h.pass_check("All Flow executions exit naturally and their empty Host retires")
            h.close_window(process)
            return

        surface = next(tab["id"] for tab in h.workspace(socket, workspace)["panes"][0]["tabs"] if tab["id"] != original)
        before = int((h.root / "progress").read_text())
        h.rpc(socket, "surface_close", pane=pane, surface=surface)
        gui.wait_for(lambda: find(Atspi.Role.LABEL, "Ready · Background"), "closed tab becomes Background")
        gui.wait_for(lambda: int((h.root / "progress").read_text()) > before + 2, "headless execution progress")
        assert Path(f"/proc/{pid}/stat").read_text().rsplit(") ", 1)[1].split()[19] == str(birth)
        h.pass_check("Closing the resumed tab preserves the same PID/birth and Background execution")
        if args.context:
            assert "no_attached_view" in context_probe(expected=False)
            assert "no_attached_view" in context_probe(expected=False, team=True)
            before_tree = h.tree(socket)
            assert "no_attached_view" in context_probe(expected=False, command=["browser", "open", "about:blank"])
            for command in (["workspace", "new", "--background", "--root", str(project)],
                            ["workspace", "focus", other_workspace],
                            ["agent", "open", agent["instance"]],
                            ["agent", "open", agent["instance"], "--pane", pane]):
                assert "no_attached_view" in context_probe(expected=False, command=command)
                assert h.tree(socket) == before_tree, "detached command changed layout"
            assert h.tree(socket) == before_tree, "detached browser open changed layout"
            h.pass_check("Background descendants report no_attached_view for identify and Team context without choosing another pane")
        h.rpc(socket, "surface_focus", pane=pane, surface=original)
        click(Atspi.Role.TOGGLE_BUTTON, "Claude sessions")
        resume()
        gui.wait_for(lambda: find(Atspi.Role.LABEL, "Opened the same running session."), "reuse existing managed session")
        h.send(socket, pane, "reopened")
        gui.wait_for(lambda: "ACK:reopened" in h.screen(socket, pane), "same-process input after resume reuse")
        assert (h.root / "starts").read_text().splitlines() == [str(pid)]
        h.pass_check("Resume of an already running history session reconnects without a second spawn")
        if args.context:
            context_probe()
            h.pass_check("Reconnecting a tab resolves its new surface on the same managed execution")

        h.close_window(process)
        before = int((h.root / "progress").read_text())
        gui.wait_for(lambda: int((h.root / "progress").read_text()) > before + 2, "progress after GUI exit")
        if args.context:
            assert "no_attached_view" in context_probe(expected=False)
            assert "no_attached_view" in context_probe(expected=False, command=["workspace", "new", "--root", str(project)])
            assert "no_attached_view" in context_probe(expected=False, command=["workspace", "focus", workspace])
            assert "no_attached_view" in context_probe(expected=False, command=["agent", "open", agent["instance"], "--pane", pane])
        process, socket = h.window("session-resume-reopened")
        gui.wait_for(lambda: f"fixture-pid:{pid}" in h.screen(socket, pane), "saved managed view restored")
        h.send(socket, pane, "after-gui-exit")
        gui.wait_for(lambda: "ACK:after-gui-exit" in h.screen(socket, pane), "input after GUI restart")
        assert (h.root / "starts").read_text().splitlines() == [str(pid)]
        assert live()[0]["process_start"] == birth
        h.pass_check("GUI exit and saved-view restoration preserve the same resumed process and accept new input")
        if args.context:
            context_probe()
            context_probe(team=True)
            context_probe(command=["read-screen"])
            h.pass_check("After GUI exit and restore, descendants resolve the new GUI socket with the same PID, birth and one spawn")
        if args.cli_controls:
            def current():
                return next(a for a in control.call("list")["agents"] if a["instance"] == agent["instance"])

            def cli_control(verb, force=False, accepted=True, instance=None):
                result = subprocess.run([args.cli, "--json", "agent", verb, instance or agent["instance"]]
                    + (["--force"] if force else []), env=h.env, text=True, capture_output=True, timeout=10)
                assert (result.returncode == 0) is accepted, result.stderr
                if accepted:
                    receipt = json.loads(result.stdout)
                    assert receipt["accepted"] and receipt["instance"] == agent["instance"]
                    assert receipt["epoch"] == agent["epoch"]
                    return receipt
                return result.stderr

            assert "not in the current Agent Host" in cli_control("terminate", instance=str(uuid.uuid4()), accepted=False)
            cli_control("terminate", force=True, accepted=False)
            assert current()["state"] == "running" and Path(f"/proc/{pid}").exists()
            h.pass_check("CLI rejects unknown executions and premature Force while preserving the live execution")
            assert cli_control("interrupt")["mode"] == "interrupt"
            gui.wait_for(lambda: (h.root / "interrupted").exists(), "CLI Ctrl+C received")
            h.send(socket, pane, "after-cli-interrupt")
            gui.wait_for(lambda: "ACK:after-cli-interrupt" in h.screen(socket, pane), "input after CLI interrupt")
            assert current()["state"] == "running" and current()["pid"] == pid
            h.pass_check("CLI interrupt reaches the existing PTY and preserves the process and input")
            keeper = host_fixture.HostClient(host_socket, "gui")
            cli_control("terminate")
            gui.wait_for(lambda: current()["state"] == "exited" and not Path(f"/proc/{pid}").exists(), "CLI normal exit and reap")
            gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Terminate agent") is None, "CLI ended row removed")
            h.pass_check("CLI normal termination reaps the exact execution and updates Agents")
            (h.root / "resist-term").touch()
            launched = subprocess.run([args.cli, "--json", "agent", "run", "--background",
                "--cwd", str(project), "--", str(h.root / "claude")],
                env=h.env, text=True, capture_output=True, check=True, timeout=30)
            agent = json.loads(launched.stdout)["agent"]
            pid = agent["pid"]
            child = int(gui.wait_for(lambda: (h.root / "owned-child").read_text(), "CLI owned child"))
            assert current()["attachment"] is None
            cli_control("terminate")
            gui.wait_for(lambda: current()["state"] == "stopping", "CLI Stopping")
            cli_control("terminate", force=True, accepted=False)
            gui.wait_for(lambda: current()["force_terminate_allowed"], "CLI Force timeout")
            assert current()["state"] == "stopping" and Path(f"/proc/{pid}").exists() and Path(f"/proc/{child}").exists()
            h.pass_check("CLI Background termination waits in Stopping after timeout without automatic killing")
            cli_control("terminate", force=True)
            gui.wait_for(lambda: current()["state"] == "exited" and not Path(f"/proc/{pid}").exists()
                         and not Path(f"/proc/{child}").exists(), "CLI forced root and child reap")
            h.rpc(socket, "surface_focus", pane=pane, surface=original)
            h.send(socket, pane, "printf 'cli-original-shell-alive\\n'")
            gui.wait_for(lambda: "cli-original-shell-alive" in h.screen(socket, pane), "CLI unrelated shell survives")
            h.pass_check("CLI explicit Force reaps Background root and owned child and preserves the original shell")
            keeper.close()
            keeper = None
        elif args.termination:
            def stop(name):
                return find(Atspi.Role.PUSH_BUTTON, name)

            def press(name):
                node = gui.wait_for(lambda: stop(name), name)
                assert node.get_action_iface().do_action(0)

            def current():
                return next(a for a in control.call("list")["agents"] if a["instance"] == agent["instance"])

            # Hold a second GUI connection so this Host stays up between executions.
            keeper = host_fixture.HostClient(host_socket, "gui")
            assert not stop("Force terminate agent")
            press("Terminate agent")
            gui.wait_for(lambda: current()["state"] == "exited", "normal termination and reap")
            gui.wait_for(lambda: not Path(f"/proc/{pid}").exists(), "normal process reaped")
            gui.wait_for(lambda: stop("Terminate agent") is None, "ended row removed")
            assert not (h.root / "owned-child").exists()
            h.pass_check("The row's stop button stops and reaps the execution without a confirmation and updates Agents")
            (h.root / "resist-term").touch()
            h.rpc(socket, "surface_focus", pane=pane, surface=original)
            launched = subprocess.run([args.cli, "--socket", str(socket), "--json", "agent", "run",
                "--pane", pane, "--cwd", str(project), "--", str(h.root / "claude")],
                env=h.env, text=True, capture_output=True, check=True, timeout=30)
            agent = json.loads(launched.stdout)["agent"]
            pid = agent["pid"]
            child = int(gui.wait_for(lambda: (h.root / "owned-child").read_text(), "owned child"))
            press("Terminate agent")
            gui.wait_for(lambda: current()["state"] == "stopping", "Stopping registry")
            gui.wait_for(lambda: find(Atspi.Role.LABEL, "Stopping · Visible"), "Stopping UI")
            force = gui.wait_for(lambda: stop("Force terminate agent"), "disabled force")
            assert not force.get_state_set().contains(Atspi.StateType.SENSITIVE)
            gui.wait_for(lambda: current()["force_terminate_allowed"], "normal termination timeout")
            gui.wait_for(lambda: (node := stop("Force terminate agent"))
                         and node.get_state_set().contains(Atspi.StateType.SENSITIVE), "enabled force")
            assert Path(f"/proc/{pid}").exists() and Path(f"/proc/{child}").exists()
            h.pass_check("Normal timeout preserves Stopping and turns the stop button into an explicit force without automatic killing")
            press("Force terminate agent")
            gui.wait_for(lambda: current()["state"] == "exited", "forced exit and reap")
            gui.wait_for(lambda: not Path(f"/proc/{pid}").exists() and not Path(f"/proc/{child}").exists(),
                         "root and owned child reaped")
            gui.wait_for(lambda: stop("Force terminate agent") is None, "forced row removed")
            h.rpc(socket, "surface_focus", pane=pane, surface=original)
            h.send(socket, pane, "printf 'original-shell-still-alive\\n'")
            gui.wait_for(lambda: "original-shell-still-alive" in h.screen(socket, pane), "original shell survived")
            h.pass_check("Force reaps root and owned child while preserving the original shell")
            keeper.close()
            keeper = None
        else:
            h.send(socket, pane, "quit")
        control.close()
        control = None
        gui.wait_for(lambda: host.poll() == 0, "natural agent exit and empty Host retirement", timeout=15)
        h.pass_check("Ended executions are reaped and the empty Host retires")
        if args.termination:
            gui.wait_for(lambda: find(Atspi.Role.PUSH_BUTTON, "Terminate agent") is None
                         and find(Atspi.Role.PUSH_BUTTON, "Force terminate agent") is None, "ended rows leave Agents")
            assert host.poll() == 0 and not host_socket.exists(), "ended rows must not restart the Host"
            h.pass_check("Ended executions leave Agents after Host shutdown without starting a Host")
        h.close_window(process)
    except Exception as error:
        diagnostic = {"error": str(error)}

        def capture(name, probe):
            try:
                diagnostic[name] = probe()
            except Exception as capture_error:
                diagnostic[name + "_error"] = str(capture_error)

        def registry():
            # The fixture's idle control connection may already have expired.
            client = host_fixture.HostClient(host_socket, "relay")
            try:
                return client.call("list")
            finally:
                client.close()

        capture("agents", registry)
        capture("tree", lambda: h.tree(socket))
        capture("nodes", lambda: [
            {"role": node.get_role_name(), "name": node.get_name(),
             "showing": node.get_state_set().contains(Atspi.StateType.SHOWING),
             "sensitive": node.get_state_set().contains(Atspi.StateType.SENSITIVE),
             "focused": node.get_state_set().contains(Atspi.StateType.FOCUSED),
             "bounds": str(component.get_extents(Atspi.CoordType.WINDOW))
                 if (component := node.get_component_iface()) else None}
            for node in find(None, all_matches=True, showing=False)])
        capture("x_focus", lambda: str(connection.get_input_focus()))
        capture("pressed_keys", lambda: list(connection.query_keymap()))

        def screenshot():
            from PIL import ImageGrab
            ImageGrab.grab(xdisplay=h.env["DISPLAY"]).save(h.root / "session-resume-failure.png")

        capture("screenshot", screenshot)
        (h.root / "session-resume-failure.json").write_text(json.dumps(diagnostic, default=str, indent=2))
        raise
    finally:
        if (args.termination or args.context) and control:
            try:
                if keeper is None:
                    keeper = host_fixture.HostClient(host_socket, "gui")
                for item in control.call("list")["agents"]:
                    if item["state"] in ("running", "stopping"):
                        control.call("terminate", instance=item["instance"], epoch=item["epoch"], mode="terminate")
                deadline = time.monotonic() + 8
                while time.monotonic() < deadline:
                    remaining = [a for a in control.call("list")["agents"] if a["state"] in ("running", "stopping")]
                    if not remaining:
                        break
                    for item in remaining:
                        if item["force_terminate_allowed"]:
                            control.call("terminate", instance=item["instance"], epoch=item["epoch"], mode="force")
                    time.sleep(.1)
                assert not remaining, remaining
            except (OSError, AssertionError) as error:
                print(f"Managed fixture cleanup: {error}", file=sys.stderr)
        if keeper:
            keeper.close()
        if control:
            control.close()
        if connection:
            connection.close()
        h.cleanup()


if __name__ == "__main__":
    main()
