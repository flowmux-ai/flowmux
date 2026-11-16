#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise product native hooks after closing a Host-owned Agent tab and GUI."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "host_fixture", Path(__file__).with_name("test-agent-host-gui.py"))
host_fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host_fixture)
fixture = host_fixture.fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--host", default="target/debug/flowmux-daemon")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for name in ("gui", "cli", "host"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = fixture.Harness(args)
    executions = []
    socket = h.root / "run/flowmux-agent-host.sock"
    try:
        options = h.root / "config/flowmux/options.json"
        options.parent.mkdir(parents=True, exist_ok=True)
        options.write_text(json.dumps({"default_shell": "/bin/sh"}))
        subprocess.run(["git", "init", "-q", str(h.root)], env=h.env, check=True)
        h.env.update(GTK_A11Y="atspi")
        h.env.pop("AT_SPI_BUS_ADDRESS", None)
        h.start_display()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        os.environ.pop("AT_SPI_BUS_ADDRESS", None)
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        desktop_mock = h.root / "desktop-mock.py"
        desktop_mock.write_text(r'''from gi.repository import Gio, GLib
import json, sys
from pathlib import Path
root = Path(sys.argv[1])
interface = Gio.DBusNodeInfo.new_for_xml("""<node><interface name="org.gtk.Notifications">
<method name="AddNotification"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sv}" direction="in"/></method>
<method name="RemoveNotification"><arg type="s" direction="in"/><arg type="s" direction="in"/></method>
</interface></node>""").interfaces[0]
def call(connection, sender, path, iface, method, parameters, invocation):
    with (root / 'desktop-events.jsonl').open('a') as out:
        out.write(json.dumps({'method': method, 'parameters': parameters.unpack()}, default=str) + '\n')
    invocation.return_value(GLib.Variant('()', ()))
connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
connection.register_object('/org/gtk/Notifications', interface, call, None, None)
Gio.bus_own_name_on_connection(connection, 'org.gtk.Notifications', Gio.BusNameOwnerFlags.NONE,
    lambda *args: (root / 'desktop-ready').touch(), None)
GLib.MainLoop().run()
''')
        h.spawn(["/usr/bin/python3", str(desktop_mock), str(h.root)])
        fixture.wait_for(lambda: (h.root / "desktop-ready").exists(), "private desktop notification service")

        def desktop_adds():
            path = h.root / "desktop-events.jsonl"
            return [json.loads(line) for line in path.read_text().splitlines()
                    if json.loads(line)["method"] == "AddNotification"] if path.exists() else []

        app, gui = h.window("native-host-hooks")

        def application_nodes():
            def walk(node):
                node.clear_cache()
                yield node
                for index in range(node.get_child_count()):
                    child = node.get_child_at_index(index)
                    if child:
                        yield from walk(child)
            desktop = Atspi.get_desktop(0)
            desktop.clear_cache()
            application = next((desktop.get_child_at_index(i) for i in range(desktop.get_child_count())
                                if desktop.get_child_at_index(i).get_process_id() == app.pid), None)
            return list(walk(application)) if application else []

        def agent_summary(provider):
            title = next((node for node in application_nodes() if node.get_name().startswith(provider + " ·")
                          and node.get_role() == Atspi.Role.LABEL
                          and node.get_state_set().contains(Atspi.StateType.SHOWING)), None)
            if title:
                parent = title.get_parent()
                return [parent.get_child_at_index(i).get_name() for i in range(parent.get_child_count())]
            return []

        def action(label):
            node = fixture.wait_for(lambda: next((node for node in application_nodes()
                if node.get_name() == label and node.get_role() == Atspi.Role.PUSH_BUTTON
                and node.get_state_set().contains(Atspi.StateType.SHOWING)), None), label)
            assert node.get_action_iface().do_action(0), label

        def picker_closed():
            return not any(node.get_role() == Atspi.Role.COMBO_BOX
                           and node.get_name() == "Agent destination"
                           for node in application_nodes())

        def ui_notifications():
            return h.rpc(gui, "notifications_list", unread_only=False)["notifications"]["entries"]
        ws = h.rpc(gui, "workspace_create", root=str(h.root), name="Native Host hooks")["workspace_created"]["id"]
        pane = h.workspace(gui, ws)["panes"][0]["id"]
        with (h.root / "host.log").open("w") as log:
            h.spawn([args.host, "--agent-host", "--socket", str(socket)], stdout=log, stderr=log)
        fixture.wait_for(socket.exists, "isolated Host")
        def host_call(op, **fields):
            client = host_fixture.HostClient(socket, "relay")
            try:
                return client.call(op, **fields)
            finally:
                client.close()
        provider_script = h.root / "provider.py"
        provider_script.write_text(r'''import ctypes, json, os, select, subprocess, sys, time, tty
from pathlib import Path
name, root, cli = sys.argv[1:]
root = Path(root)
ctypes.CDLL(None).prctl(15, name.encode(), 0, 0, 0)
(root / (name + '-env')).write_text(json.dumps(dict(os.environ)))
with (root / (name + '-starts')).open('a') as f: f.write(str(os.getpid()) + '\n')
tty.setraw(0)
print('EXECUTION:' + name + ':' + str(os.getpid()) + '\r', flush=True)
pending = b''
counter = 0
while True:
    counter += 1
    progress = root / (name + '-progress')
    progress.with_suffix('.tmp').write_text(str(counter))
    progress.with_suffix('.tmp').replace(progress)
    frame_path = root / (name + '-frame')
    if frame_path.exists():
        frame = json.loads(frame_path.read_text())
        frame_path.unlink()
        print('\x1b]2;' + frame['title'] + '\x07\x1b[2J\x1b[H' + frame['screen'].replace('\n', '\r\n'), end='', flush=True)
        (root / (name + '-frame-seen')).write_text(frame['id'])
    command = root / (name + '-command')
    if command.exists():
        task = json.loads(command.read_text())
        command.unlink()
        env = dict(os.environ, **task.get('env', {}))
        argv = [cli, *task['cli_args']] if 'cli_args' in task else [cli, 'hooks', name, task['event']]
        if task.get('nested'):
            argv = [sys.executable, '-c',
                'import ctypes,subprocess,sys;ctypes.CDLL(None).prctl(15,sys.argv[1].encode(),0,0,0);'
                'sys.exit(subprocess.run(sys.argv[2:]).returncode)', name, *argv]
        input_text = task.get('stdin', json.dumps(task['payload']))
        if task.get('stream_delay'):
            process = subprocess.Popen(argv, env=env, text=True, stdin=subprocess.PIPE,
                                       stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            time.sleep(task['stream_delay'])
            stdout, stderr = process.communicate(input_text, timeout=8)
            result = subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)
        else:
            result = subprocess.run(argv, input=input_text, env=env, text=True, capture_output=True, timeout=8)
        (root / (name + '-result')).write_text(json.dumps({'id': task['id'], 'code': result.returncode,
                                                        'stdout': result.stdout, 'stderr': result.stderr}))
    if select.select([0], [], [], .05)[0]:
        data = os.read(0, 4096)
        if not data: break
        pending += data.replace(b'\r', b'\n')
        while b'\n' in pending:
            line, pending = pending.split(b'\n', 1)
            if line == b'quit': sys.exit(0)
            print('ACK:<' + line.decode() + '>\r', flush=True)
''')
        stale = {"FLOWMUX_SOCKET_PATH": str(gui), "FLOWMUX_SOCKET": str(gui),
                 "FLOWMUX_PANE_ID": pane, "FLOWMUX_SURFACE_ID": str(uuid.uuid4()),
                 "FLOWMUX_WORKSPACE_ID": ws, "FLOWMUX_TAB_ID": ws,
                 "FLOWMUX_AGENT_PID": "1", "FLOWMUX_AGENT_NAME": "wrong"}
        for name in ("claude", "codex", "gemini", "opencode", "cline", "antigravity"):
            program = h.root / ("agy" if name == "antigravity" else name)
            program.symlink_to("/usr/bin/python3")
            launched = subprocess.run(
                [args.cli, "--socket", str(gui), "--json", "agent", "run", "--pane", pane, "--cwd", str(h.root),
                 "--", str(program), "-u", str(provider_script), name, str(h.root), args.cli],
                env=dict(h.env, **stale), capture_output=True, text=True, check=True, timeout=30)
            result = json.loads(launched.stdout)
            agent = result["agent"]
            executions.append(agent)
            birth = Path(f"/proc/{agent['pid']}/stat").read_text().rsplit(") ", 1)[1].split()[19]
            fixture.wait_for(lambda: (h.root / (name + "-env")).exists(), "provider environment")
            env = json.loads((h.root / (name + "-env")).read_text())
            assert env["FLOWMUX_AGENT_INSTANCE_ID"] == agent["instance"]
            assert env["FLOWMUX_AGENT_HOST_EPOCH"] == agent["epoch"]
            assert env["FLOWMUX_SOCKET_PATH"] == str(socket)
            assert all(key not in env for key in stale if key != "FLOWMUX_SOCKET_PATH"), env.keys()

            def hook(event, session="native-session", **overrides):
                expected = overrides.pop("expected", 0)
                task = {"id": uuid.uuid4().hex, "event": event,
                        "payload": {"session_id": session, "turn_id": "turn-1", **overrides.pop("payload", {})},
                        **overrides}
                path = h.root / (name + "-command")
                temporary = path.with_suffix(".tmp")
                temporary.write_text(json.dumps(task))
                temporary.replace(path)
                result_path = h.root / (name + "-result")
                def received():
                    try:
                        result = json.loads(result_path.read_text())
                        return result if result["id"] == task["id"] else None
                    except (FileNotFoundError, json.JSONDecodeError):
                        return None
                result = fixture.wait_for(received, "native hook finishes", timeout=task.get("stream_delay", 0) + 15)
                assert (result["code"] == 0) == (expected == 0), result
                return result

            def presence():
                return next(item for item in host_call("list")["agents"]
                            if item["instance"] == agent["instance"])["presence"]

            fixture.wait_for(lambda: agent_summary(name), "registry is visible before native hooks")
            desktop_before = len(desktop_adds())
            hook("session-start")
            fixture.wait_for(lambda: presence() and presence()["session_id"] == "native-session", "native SessionStart reaches Host")
            hook("prompt-submit" if name == "claude" else "turn-start")
            assert presence()["status"] == "working", presence()
            hook("permission-request" if name == "claude" else "notification",
                 payload={"notification_type": "permission_prompt", "message": "Approval needed"})
            assert presence()["status"] == "blocked", presence()
            hook("session-start")
            assert presence()["status"] == "blocked", "duplicate start cleared permission wait"
            surface = result["view"]["surface_created"]["id"]
            h.rpc(gui, "surface_close", pane=pane, surface=surface)
            assert presence()["status"] == "blocked", "tab close cleared Host wait"
            def host_notifications():
                return [n for n in host_call("list")["notifications"] if n["instance"] == agent["instance"]]
            notices = host_notifications()
            assert len(notices) == 1 and notices[0]["notification"]["level"] == "needs_input", notices
            assert notices[0]["notification"]["body"] == "Approval needed", notices
            notice_id = notices[0]["notification"]["id"]
            fixture.wait_for(lambda: any(n["id"] == notice_id and n["managed"]["instance"] == agent["instance"]
                                        for n in ui_notifications()), "Background notification appears in the bell")
            fixture.wait_for(lambda: len(desktop_adds()) == desktop_before + 1, "one causal desktop notification")
            if name == "claude":
                h.rpc(gui, "notification_open", notification_id=notice_id)
                action("Cancel")
                fixture.wait_for(picker_closed, "notification picker cancelled")
                assert next(n for n in host_call("list")["agents"] if n["instance"] == agent["instance"])["view"] is None
                h.rpc(gui, "notification_open", notification_id=notice_id)
                action("Open agent")
                fixture.wait_for(lambda: next((n["view"] for n in host_call("list")["agents"]
                    if n["instance"] == agent["instance"] and n.get("view")), None), "notification reconnects Background execution")
                right = h.rpc(gui, "pane_split", pane=pane, direction="vertical")["pane_split_done"]["new_pane"]
                moved = h.rpc(gui, "agent_view_open", pane=right, instance=agent["instance"])["surface_created"]["id"]
                fixture.wait_for(lambda: any(n["id"] == notice_id and n.get("pane") == right for n in ui_notifications()), "notification tracks moved execution")
                from Xlib import X, XK, display
                from Xlib.ext import xtest
                connection = display.Display(h.env["DISPLAY"])
                try:
                    pid_atom = connection.intern_atom("_NET_WM_PID")
                    native = next(window for window in connection.screen().root.query_tree().children
                        if (pid := window.get_full_property(pid_atom, X.AnyPropertyType)) is not None
                        and int(pid.value[0]) == app.pid and window.get_geometry().width > 100)
                    native.set_input_focus(X.RevertToParent, X.CurrentTime)
                    connection.sync()
                    time.sleep(.3)
                    review_keys = [connection.keysym_to_keycode(XK.string_to_keysym(key))
                                   for key in ("Control_L", "Alt_L", "e")]
                    for keycode in review_keys:
                        xtest.fake_input(connection, X.KeyPress, keycode)
                    for keycode in reversed(review_keys):
                        xtest.fake_input(connection, X.KeyRelease, keycode)
                    connection.sync()
                    def review_visible():
                        return any(node.get_name() == "Back to previous tab"
                                   and node.get_state_set().contains(Atspi.StateType.SHOWING)
                                   for node in application_nodes())
                    fixture.wait_for(review_visible, "Code Review covers the moved execution")
                    h.rpc(gui, "pane_focus", pane=pane)
                    h.rpc(gui, "notification_open", notification_id=notice_id)
                    fixture.wait_for(lambda: not review_visible(), "notification reveals the execution from Code Review")
                    time.sleep(.3)  # GTK completes focus-enter after the RPC reply.
                    for character in [*"notice", "Return"]:
                        keycode = connection.keysym_to_keycode(XK.string_to_keysym(character))
                        xtest.fake_input(connection, X.KeyPress, keycode)
                        xtest.fake_input(connection, X.KeyRelease, keycode)
                    connection.sync()
                finally:
                    connection.close()
                fixture.wait_for(lambda: "ACK:<notice>" in h.screen(gui, right), "notification focuses the moved pane for native input")
                fixture.wait_for(lambda: host_notifications()[0]["notification"]["read"], "Host acknowledges notification reads")
                h.rpc(gui, "notifications_clear")
                fixture.wait_for(lambda: not host_notifications(), "Host dismisses cleared notification")
                hook("permission-request", payload={"notification_type": "permission_prompt", "message": "Approval needed"})
                assert not host_notifications(), "repeat resurrected dismissed notification"
                h.rpc(gui, "surface_close", pane=right, surface=moved)
                assert (h.root / (name + "-starts")).read_text().splitlines() == [str(agent["pid"])]
                h.pass_check("notification preserves picker cancel, follows a moved view out of Code Review with native input, and keeps read/dismiss across duplicate hooks")
            h.pass_check(name + ": native wait creates one Host/bell/desktop notification after tab closure")
            if name == "codex":
                child = {"agent_id": "child", "turn_id": "child-old"}
                hook("subagent-start", payload=child)
                hook("subagent-stop", payload=child)
                hook("stop")
                fixture.wait_for(lambda: presence()["status"] == "idle", "root Stop settles")
                completed = presence()
                assert completed["turn_outcome"] == "completed", completed
                completed_notice_ids = {n["notification"]["id"] for n in host_notifications()}
                summary = "Response completed · Background"
                fixture.wait_for(lambda: summary in agent_summary("codex"), "Agents shows completed Background execution")
                for event in ("running", "turn-start", "stop"):
                    hook(event)
                    assert presence() == completed, (event, presence(), completed)
                fixture.wait_for(lambda: summary in agent_summary("codex"), "late hooks preserve visible completion")
                assert {n["notification"]["id"] for n in host_notifications()} == completed_notice_ids
                h.pass_check("Codex: late root progress/start/Stop preserve completed Background status in Host and visible Agents row")
            before = int((h.root / (name + "-progress")).read_text())
            h.close_window(app)
            if name == "claude":
                headless_desktop_before = len(desktop_adds())
                current_presence = presence()
                original_ids = {n["notification"]["id"] for n in host_notifications()}
                for fields in (
                    {"cli_args": ["notify", "--pane", pane, "refuse explicit pane"]},
                    {"cli_args": ["--socket", str(h.root / "missing-gui.sock"), "notify", "refuse foreign GUI"]},
                    {"cli_args": ["notify", "wrong epoch"], "env": {"FLOWMUX_AGENT_HOST_EPOCH": str(uuid.uuid4())}},
                    {"cli_args": ["notify", "invalid instance"], "env": {"FLOWMUX_AGENT_INSTANCE_ID": "invalid"}},
                    {"cli_args": ["notify", "missing Host"], "env": {"FLOWMUX_SOCKET_PATH": str(h.root / "absent-host.sock")}},
                ):
                    hook("explicit", expected=1, **fields)
                external = subprocess.run([args.cli, "notify", "copied identity"], env=dict(h.env, **env),
                                          capture_output=True, text=True, timeout=8)
                assert external.returncode != 0 and "does not belong" in external.stderr, external.stderr
                assert {n["notification"]["id"] for n in host_notifications()} == original_ids
                h.pass_check("managed explicit notify rejects copied identity, wrong epoch/instance/socket and explicit pane without fallback")
                hook("explicit", cli_args=["notify", "--title", "Headless explicit", "--level", "attention", "Build needs a choice"],
                     env={"FLOWMUX_PANE_ID": str(uuid.uuid4()), "FLOWMUX_SURFACE_ID": str(uuid.uuid4()),
                          "CODEX_THREAD_ID": "inherited-unrelated-session"})
                hook("explicit", cli_args=["--socket", str(socket), "notify-complete", "--agent", "Build", "--message", "Artifact ready"])
                osc = "\x1b]777;notify;Headless stream;Stream message\x07"
                streamed = hook("explicit", cli_args=["notify-stream"], stdin=osc, stream_delay=32)
                assert streamed["stdout"] == osc, streamed
                notices = host_notifications()
                explicit = [n for n in notices if n["notification"]["id"] not in original_ids]
                assert len(explicit) == 3, explicit
                assert {n["notification"]["title"] for n in explicit} == {"Headless explicit", "Build ready", "Headless stream"}
                assert all(n["notification"]["source_pane"] is None for n in explicit)
                assert presence() == current_presence, "explicit notification altered native lifecycle state"
                assert len(desktop_adds()) == headless_desktop_before, "headless notification tried to send a desktop effect"
                h.pass_check("notify, notify-complete and notify-stream retain headless notifications and unchanged lifecycle; stream survives 32 seconds of silence with exact output")
            if name in ("gemini", "antigravity", "cline"):
                def frame(screen, title=""):
                    token = uuid.uuid4().hex
                    path = h.root / (name + "-frame")
                    temporary = path.with_suffix(".tmp")
                    temporary.write_text(json.dumps({"id": token, "screen": screen, "title": title}))
                    temporary.replace(path)
                    seen = h.root / (name + "-frame-seen")
                    fixture.wait_for(lambda: seen.exists() and seen.read_text() == token, "provider renders a live frame")

                hook("session-start", session="screen-session")
                hook("turn-start", session="screen-session")
                frame("Type your message", "⠋ Executing Hook")
                if name != "cline":
                    hook("stop", session="screen-session")
                time.sleep(1.1)
                assert presence()["status"] == "working", "working title completed the response"
                if name != "cline":
                    hook("turn-start", session="screen-session")
                    frame("Type your message")
                    time.sleep(1.1)
                    assert presence()["status"] == "working", "old Stop completed the continuation"
                    frame("Type your message", "⠋ Executing Hook")
                    hook("stop", session="screen-session")
                    time.sleep(1.1)
                    assert presence()["status"] == "working"
                    frame("Type your message")
                    expected = "completed"
                else:
                    frame("⠋ Thinking... (esc to cancel)\nType your message")
                    time.sleep(.6)
                    frame("* Error: Fixture unauthorized\nAsk anything")
                    expected = "failed"
                fixture.wait_for(lambda: presence()["status"] == "idle", "Host settles the stable ready screen with no GUI")
                assert presence()["turn_outcome"] == expected, presence()
                count = len(host_notifications())
                time.sleep(1.1)
                assert len(host_notifications()) == count, "screen poll duplicated completion"
                assert Path(f"/proc/{agent['pid']}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
                assert not next(n for n in host_call("list")["agents"] if n["instance"] == agent["instance"])["view"]
                app, gui = h.window("headless-screen-result-" + name)
                outcome_label = "Response completed" if expected == "completed" else "Response failed"
                fixture.wait_for(lambda: outcome_label + " · Background" in agent_summary(name),
                                 "Agents displays the result observed without any GUI")
                h.close_window(app)
                if name != "cline":
                    hook("session-start", session="canceled-screen")
                    hook("turn-start", session="canceled-screen")
                    frame("ℹ Request cancelled.\nType your message")
                    if name == "gemini":
                        fixture.wait_for(lambda: presence()["status"] == "idle", "headless cancellation settles")
                        assert presence()["turn_outcome"] == "canceled", presence()
                h.pass_check(name + ": headless live screen/title settlement preserves continuation, outcome, process identity and notification deduplication")
            if name == "codex":
                child = {"agent_id": "child", "turn_id": "child-new"}
                hook("running", payload=child)
                assert presence()["status"] == "working", "reused child work was ignored"
                hook("stop", payload={"stop_hook_active": True})
                hook("running")
                assert presence()["status"] == "working", "explicit root retry was ignored"
                hook("stop")
                assert presence()["status"] == "working", "parent settled before active child"
                hook("subagent-stop", payload=child)
                fixture.wait_for(lambda: presence()["status"] == "idle", "explicit retry settles after child without a GUI")
                assert {n["notification"]["id"] for n in host_notifications()} == completed_notice_ids, "retry duplicated completion notification"
                h.pass_check("Codex: explicit stop-hook retry and reused child work settle without any GUI or replacement process")
            hook("session-start", session="replacement")
            assert presence()["session_id"] == "replacement", presence()
            hook("stop")
            hook("session-end")
            assert presence()["session_id"] == "replacement", "old session altered replacement"
            for overrides in ({"env": {"FLOWMUX_AGENT_HOST_EPOCH": str(uuid.uuid4())}},
                              {"env": {"FLOWMUX_AGENT_INSTANCE_ID": "invalid"}}, {"nested": True}):
                hook("session-end", session="replacement", **overrides)
                assert presence()["session_id"] == "replacement", "foreign root/epoch was accepted"
            hook("session-end", session="replacement")
            assert presence() is None
            hook("prompt-submit" if name == "claude" else "turn-start", session="replacement")
            assert presence() is None, "late hook resurrected ended session"
            hook("prompt-submit" if name == "claude" else "turn-start", session="never-started")
            assert presence() is None, "unstarted session bypassed the native boundary"
            hook("session-start", session="next")
            hook("prompt-submit" if name == "claude" else "turn-start", session="next")
            assert presence()["status"] == "working", presence()
            assert int((h.root / (name + "-progress")).read_text()) > before
            assert Path(f"/proc/{agent['pid']}/stat").read_text().rsplit(") ", 1)[1].split()[19] == birth
            hook("permission-request" if name == "claude" else "notification", session="next",
                 payload={"notification_type": "permission_prompt", "message": "Headless approval needed"})
            retained = host_notifications()
            assert any(n["notification"]["body"] == "Headless approval needed" for n in retained), retained
            desktop_before_reopen = len(desktop_adds())
            app, gui = h.window("native-host-hooks-reopened-" + name)
            fixture.wait_for(lambda: {n["notification"]["id"] for n in retained}.issubset({n["id"] for n in ui_notifications()}),
                             "headless notification history restores into the bell")
            time.sleep(.3)
            assert len(desktop_adds()) == desktop_before_reopen, "GUI reconnection replayed old desktop notifications"
            h.pass_check(name + ": GUI imports headless notification history without desktop replay")
            h.rpc(gui, "agent_view_open", pane=pane, instance=agent["instance"])
            if name == "claude":
                fixture.wait_for(lambda: next(n["view"] for n in host_call("list")["agents"]
                    if n["instance"] == agent["instance"]), "reattached execution reports its current view")
                hook("explicit", cli_args=["--socket", str(gui), "notify", "--title", "Attached explicit", "Current execution"],
                     env={"FLOWMUX_PANE_ID": str(uuid.uuid4()), "FLOWMUX_SURFACE_ID": str(uuid.uuid4())})
                new_notice = fixture.wait_for(lambda: next((n for n in ui_notifications()
                    if n["title"] == "Attached explicit" and n.get("pane") == pane), None), "explicit notification tracks the verified reattached view")
                h.rpc(gui, "notification_open", notification_id=new_notice["id"])
                h.pass_check("attached explicit notification accepts only the current GUI socket and opens the same execution despite stale pane variables")
            token = name + "-" + uuid.uuid4().hex
            h.send(gui, pane, token)
            fixture.wait_for(lambda: "ACK:<" + token + ">" in "".join(h.screen(gui, pane).split()),
                             "fresh same-process input")
            assert (h.root / (name + "-starts")).read_text().splitlines() == [str(agent["pid"])]
            h.pass_check(name + ": product native hooks survive tab/GUI closure; sessions, root identity and fresh reattached input verified")
    finally:
        try:
            if socket.exists():
                try:
                    executions = host_call("list")["agents"]
                except (OSError, AssertionError):
                    pass
                for agent in executions:
                    assert agent["cwd"] == str(h.root), agent
                    try:
                        host_call("terminate", instance=agent["instance"], epoch=agent["epoch"], mode="terminate")
                    except (OSError, AssertionError):
                        pass
                fixture.wait_for(lambda: not socket.exists(), "fixture Host reaps its executions", timeout=10)
        finally:
            h.cleanup()


if __name__ == "__main__":
    main()
