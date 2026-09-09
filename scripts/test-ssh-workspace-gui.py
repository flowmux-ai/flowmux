#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise a real flowmux GUI against ssh-workspace-fixture.py --keep.

Uses its own Xvfb, D-Bus, XDG directories and process handles. HOME is unchanged.
Requires python3-xlib to close test windows through WM_DELETE_WINDOW.
Artifacts remain in the printed temporary directory, including on failure.
--keep leaves the isolated windows open until Ctrl+C/SIGTERM.
"""

import argparse
from contextlib import closing
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
from pathlib import Path
import re
import select
import shlex
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request
import uuid

from Xlib import X, display, protocol


def wait_for(check, description, timeout=30):
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            value = check()
            if value:
                return value
        except (OSError, RuntimeError, ValueError) as error:
            last = error
        time.sleep(0.1)
    raise AssertionError(f"Timed out: {description}; last error: {last}")


def alive(pid):
    try:
        os.kill(pid, 0)
        return True
    except ProcessLookupError:
        return False


class Harness:
    def __init__(self, args):
        self.args = args
        self.root = Path(tempfile.mkdtemp(prefix="fm-gui-"))
        self.events = (self.root / "events.jsonl").open("w", buffering=1)
        self.children = []
        self.sessions = set()
        self.remote_files = set()
        self.protected_pids = {pid for pid in args.protected_pid if alive(pid)}
        self.env = {key: value for key, value in os.environ.items()
                    if not key.startswith("FLOWMUX_") and key not in
                    ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "FLATPAK_ID")}
        for variable, folder in [("XDG_RUNTIME_DIR", "run"), ("XDG_CONFIG_HOME", "config"),
                                 ("XDG_STATE_HOME", "state"), ("XDG_DATA_HOME", "data"),
                                 ("XDG_CACHE_HOME", "cache")]:
            path = self.root / folder
            path.mkdir(mode=0o700)
            self.env[variable] = str(path)
        self.env.update(GDK_BACKEND="x11", GTK_A11Y="none", GSK_RENDERER="cairo",
                        LIBGL_ALWAYS_SOFTWARE="1", HISTFILE="/dev/null",
                        PATH=str(Path(args.cli).parent) + os.pathsep + self.env.get("PATH", ""))
        assert self.env.get("HOME") == os.environ.get("HOME")
        self.state_path = self.root / "state/flowmux/state.json"
        print(f"ARTIFACTS: {self.root}", flush=True)

    def log(self, event, **fields):
        self.events.write(json.dumps({"time": time.time(), "event": event, **fields}) + "\n")

    def pass_check(self, text):
        print(f"PASS: {text}", flush=True)
        self.log("pass", check=text)

    def spawn(self, argv, **kwargs):
        process = subprocess.Popen(argv, env=self.env, start_new_session=True, **kwargs)
        assert process.pid not in self.protected_pids
        self.children.append(process)
        self.log("spawn", pid=process.pid, argv=argv)
        return process

    def stop(self, process):
        assert process in self.children and process.pid not in self.protected_pids
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        self.assert_protected()

    def close_window(self, process):
        assert process in self.children and process.pid not in self.protected_pids
        assert process.args[0] == self.args.gui
        # Ask only this process's windows on our private Xvfb to close.
        # Normal GTK shutdown flushes state and LLVM coverage profiles;
        # SIGTERM skips both.
        with closing(display.Display(self.env["DISPLAY"])) as connection:
            pid_atom = connection.intern_atom("_NET_WM_PID")
            for window in connection.screen().root.query_tree().children:
                pid = window.get_full_property(pid_atom, X.AnyPropertyType)
                if pid is not None and int(pid.value[0]) == process.pid:
                    window.send_event(protocol.event.ClientMessage(
                        window=window, client_type=connection.intern_atom("WM_PROTOCOLS"),
                        data=(32, [connection.intern_atom("WM_DELETE_WINDOW"), X.CurrentTime, 0, 0, 0])))
            connection.sync()
        process.wait(timeout=20)
        assert process.returncode == 0, f"GUI exited with {process.returncode}"
        self.assert_protected()

    def assert_protected(self):
        for pid in self.protected_pids:
            assert alive(pid), f"Protected user GUI {pid} exited during test"

    def start_display(self):
        read_fd, write_fd = os.pipe()
        with (self.root / "xvfb.log").open("w") as log:
            self.spawn(["Xvfb", "-displayfd", str(write_fd), "-screen", "0", "1600x1000x24", "-nolisten", "tcp"],
                       pass_fds=(write_fd,), stdout=log, stderr=log)
        os.close(write_fd)
        try:
            assert select.select([read_fd], [], [], 10)[0], "Xvfb failed to report display"
            self.env["DISPLAY"] = ":" + os.read(read_fd, 64).decode().strip()
        finally:
            os.close(read_fd)
        with (self.root / "dbus.log").open("w") as log:
            bus = self.spawn(["dbus-daemon", "--session", "--nofork", "--print-address=1"], stdout=subprocess.PIPE, stderr=log)
        assert select.select([bus.stdout], [], [], 10)[0], "D-Bus failed to report address"
        self.env["DBUS_SESSION_BUS_ADDRESS"] = bus.stdout.readline().decode().strip()

    def window(self, name):
        with (self.root / f"{name}.log").open("w") as log:
            process = self.spawn([self.args.gui], stdout=log, stderr=log, cwd=self.root)
        path = self.root / f"run/flowmux-{process.pid}.sock"
        wait_for(lambda: path.exists() and self.rpc(path, "ping"), f"{name} GUI socket")
        return process, path

    def rpc(self, path, verb, **fields):
        assert path.parent == self.root / "run"
        request = {"id": 1, "kind": "request", "verb": verb, **fields}
        self.log("request", socket=str(path), request=request)
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(20)
            stream.connect(str(path))
            stream.sendall((json.dumps(request) + "\n").encode())
            line = stream.makefile("rb").readline()
            if not line:
                raise RuntimeError(f"GUI closed IPC during {verb}; see {self.root}/*.log")
            response = json.loads(line)
        self.log("rpc", socket=str(path), request=request, response=response)
        if "error" in response:
            raise RuntimeError(str(response["error"]))
        return response

    def ssh(self, path, op, **fields):
        return self.rpc(path, "ssh", request={"op": op, **fields})["ssh"]["value"]

    def tree(self, path):
        return self.rpc(path, "workspace_tree")["tree"]["workspaces"]

    def workspace(self, path, workspace):
        return next(ws for ws in self.tree(path) if ws["id"] == workspace)

    def connected(self, path, workspace):
        return wait_for(lambda: self.ssh(path, "status", workspace=workspace)["state"] == "connected", "SSH connected")

    def create(self, path, tmux=False):
        result = self.ssh(path, "create", request_id=str(uuid.uuid4()), name="GUI SSH fixture",
                          config={"target": {"host": self.args.host, "config_file": self.args.ssh_config},
                                  "cwd": self.args.remote_cwd, "tmux": tmux, "forwards": []}, command=[])
        workspace = result["workspace"]
        self.connected(path, workspace)
        ws = self.workspace(path, workspace)
        if tmux:
            for pane, tab in self.terminals(ws):
                self.sessions.add("flowmux-" + tab["id"].replace("-", ""))
        return workspace

    @staticmethod
    def terminals(workspace):
        return [(pane["id"], tab) for pane in workspace["panes"] for tab in pane["tabs"] if tab["kind"] == "ssh_terminal"]

    def screen(self, path, pane):
        return self.rpc(path, "pane_read_screen", pane=pane)["screen_contents"]["text"]

    def send(self, path, pane, command):
        return self.rpc(path, "pane_send_keys", pane=pane, keys=command + "\n")

    def marker(self, path, pane, tab, cwd):
        workspace = next(ws["id"] for ws in self.tree(path)
                         if any(item["id"] == pane for item in ws["panes"]))
        self.rpc(path, "workspace_focus", workspace=workspace)
        self.rpc(path, "surface_focus", pane=pane, surface=tab)
        token = "FM" + uuid.uuid4().hex
        self.send(path, pane, "HISTFILE=/dev/null; printf '" + token + ":%s:%s:%s:%s\\n' "
                  '"$PWD" "${FLOWMUX_SSH_FIXTURE-unset}" "${FLOWMUX_SOCKET_PATH-unset}" "$$"')
        expected = token + ":" + cwd + ":" + str(Path(self.args.ssh_config).parent) + ":unset:"
        def observed():
            text = self.screen(path, pane).replace("\r", "").replace("\n", "")
            match = re.search(re.escape(expected) + r"(\d+)", text)
            return match.group(1) if match else None
        return wait_for(observed, f"remote shell marker {pane}/{tab}")

    def check_hangul(self, path, pane):
        token = "UTF8" + uuid.uuid4().hex
        # Remove one complete Hangul character through readline, then print it.
        self.send(path, pane, "printf '" + token + ":<%s>\\n' '한글테스틀\x7f트'")
        wait_for(lambda: token + ":<한글테스트>" in self.screen(path, pane),
                 "Hangul input, backspace, and output round trip")

    def remote(self, command, check=True):
        result = subprocess.run(["ssh", "-F", self.args.ssh_config, "-o", "BatchMode=yes", self.args.host, command],
                                env=self.env, capture_output=True, text=True, timeout=15)
        if check and result.returncode:
            raise RuntimeError(result.stderr)
        return result

    def persisted(self, workspace, tab_ids):
        def saved():
            state = json.loads(self.state_path.read_text())
            found = next((ws for ws in state["workspaces"] if ws["id"] == workspace), None)
            return found if found and all(tab in json.dumps(found) for tab in tab_ids) else None
        return wait_for(saved, "workspace persisted")

    def check_removed_preview(self, path, workspace):
        forward = str(uuid.uuid4())
        status = self.ssh(path, "forward_add", workspace=workspace,
                          spec={"id": forward, "remote_port": self.args.http_port,
                                "local_port": None, "https": False})
        port = next(item["local_port"] for item in status["forwards"] if item["id"] == forward)
        url = f"http://127.0.0.1:{port}"
        preview = self.ssh(path, "preview", workspace=workspace, id=forward)
        pane = preview["pane"]
        self.rpc(path, "surface_focus", pane=pane, surface=preview["surface"])
        wait_for(lambda: self.rpc(path, "browser_url", pane=pane)["browser_result"]["value"].startswith(url),
                 "preview to remove loaded")
        status = self.ssh(path, "forward_remove", workspace=workspace, id=forward)
        assert all(item["id"] != forward for item in status["forwards"])
        requests = []

        class Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                requests.append(self.path)
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"unexpected reused port")

            def log_message(self, *_):
                pass

        with HTTPServer(("127.0.0.1", port), Handler) as server:
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            try:
                for verb, fields in [("browser_navigate", {"url": url + "/expired"}),
                                     ("browser_back", {}), ("browser_reload", {})]:
                    try:
                        self.rpc(path, verb, pane=pane, **fields)
                    except RuntimeError:
                        pass  # Removing the forward may replace the WebView with a placeholder.
                time.sleep(1)
                assert not requests, f"Expired preview reached reused port: {requests}"
                try:
                    value = self.rpc(path, "browser_url", pane=pane)["browser_result"]["value"]
                    assert value in ("", "about:blank"), value
                except RuntimeError:
                    pass
            finally:
                server.shutdown()
                thread.join(timeout=5)
        self.pass_check("removing an active forward expires preview; navigate/back/reload cannot reach reused port")

    def check_one_shot(self, path):
        remote_file = self.args.remote_cwd + "/gui-once-" + uuid.uuid4().hex
        self.remote_files.add(remote_file)
        request = {"request_id": str(uuid.uuid4()), "name": "GUI one-shot fixture",
                   "config": {"target": {"host": self.args.host, "config_file": self.args.ssh_config},
                              "cwd": self.args.remote_cwd, "tmux": False, "forwards": []},
                   "command": ["sh", "-c", "printf 'once\\n' >> " + shlex.quote(remote_file)]}
        before = {ws["id"] for ws in self.tree(path)}
        workspace = self.ssh(path, "create", **request)["workspace"]
        assert self.ssh(path, "create", **request)["workspace"] == workspace
        self.connected(path, workspace)
        pane, tab = self.terminals(self.workspace(path, workspace))[0]
        self.marker(path, pane, tab["id"], self.args.remote_cwd)
        assert self.remote("cat -- " + shlex.quote(remote_file)).stdout == "once\n"
        assert {ws["id"] for ws in self.tree(path)} == before | {workspace}
        self.ssh(path, "disconnect", workspace=workspace)
        self.ssh(path, "connect", workspace=workspace)
        self.connected(path, workspace)
        assert self.ssh(path, "create", **request)["workspace"] == workspace
        self.marker(path, pane, tab["id"], self.args.remote_cwd)
        assert self.remote("cat -- " + shlex.quote(remote_file)).stdout == "once\n"
        assert {ws["id"] for ws in self.tree(path)} == before | {workspace}
        self.pass_check("one-shot command appends once; request-id replay and disconnect/reconnect never rerun it")

    def check_agent_fallback(self, path):
        workspace = self.create(path)
        pane, tab = self.terminals(self.workspace(path, workspace))[0]
        surface = tab["id"]
        control = self.args.remote_cwd + "/gui-agent-" + uuid.uuid4().hex
        self.remote_files.add(control)
        # A bounded remote TUI lets hidden panes change without keyboard focus,
        # real agent credentials, or touching another user's running process.
        program = """import json, pathlib, sys, time
control = pathlib.Path(sys.argv[1])
sys.stdout.write('\\x1b[?1049h')
last = None
deadline = time.monotonic() + 120
while time.monotonic() < deadline:
    try:
        value = json.loads(control.read_text())
    except (OSError, ValueError):
        time.sleep(.05)
        continue
    if value != last:
        if value.get('exit'):
            break
        sys.stdout.write('\\x1b[2J\\x1b[3J\\x1b[H\\x1b]2;' + value['title'] + '\\x07' + value['text'])
        sys.stdout.flush()
        last = value
    time.sleep(.05)
"""

        def frame(title="fixture shell", text="ordinary remote output\r\n", **extra):
            value = json.dumps({"title": title, "text": text, **extra})
            self.remote("printf %s " + shlex.quote(value) + " > " + shlex.quote(control))

        def start():
            self.marker(path, pane, surface, self.args.remote_cwd)
            self.send(path, pane, "exec python3 -u -c " + shlex.quote(program) + " " + shlex.quote(control))

        def agent():
            return next(t for _, t in self.terminals(self.workspace(path, workspace))
                        if t["id"] == surface).get("agent")

        def detected(name, status):
            def matches():
                value = agent()
                return value if value and value["name"] == name and value["status"] == status else None
            value = wait_for(matches, f"remote {name}/{status} screen fallback")
            assert value["source"] == "flowmux:screen", value
            for field in ("pid", "session_id", "session_name", "messaging_socket"):
                assert value.get(field) is None, value

        frame(text="Codex working\r\nWorking (1s • esc to interrupt)\r\n")
        start()
        detected("codex", "working")
        # Real Codex keeps its name in the composer, not the progress row;
        # tmux commonly supplies a directory title instead of an agent name.
        composer = "› Ask Codex to do anything\r\n  gpt-6-astra high · /srv/project\r\n"
        frame("remote workdir", composer)
        detected("codex", "idle")
        for second, bullet in enumerate("•◦•◦", 1):
            frame("remote workdir", f"{bullet} Working ({second}s • esc to interrupt)\r\n\r\n" + composer)
            detected("codex", "working")
            for _ in range(8):
                value = agent()
                assert value and value["name"] == "codex" and value["status"] == "working", value
                time.sleep(.1)
        for spinner in "⠋⠹⠸⠼":
            frame("remote workdir", "• 1 일\r\n  2 이\r\n" + composer +
                  f'\x1b[24;1H[flowmux-90:node*  "{spinner} remote workdir" 14:25 09-Sep-26\x1b[4;1H')
            detected("codex", "working")
            for _ in range(8):
                value = agent()
                assert value and value["name"] == "codex" and value["status"] == "working", value
                time.sleep(.1)
        frame("remote workdir", composer)
        detected("codex", "idle")
        self.pass_check("Codex stays working across both animated bullets and tmux streaming title frames, then returns idle")
        for title, name in [("Claude", "claude"), ("Codex", "codex"),
                            ("OpenCode", "opencode"), ("Cline", "cline"),
                            ("agy", "antigravity")]:
            frame(title + " working")
            detected(name, "working")
        self.pass_check("SSH screen text and all five agent OSC titles register screen-only identity without session metadata")

        hidden = self.rpc(path, "surface_create", workspace=workspace, cwd=None)["surface_created"]
        self.marker(path, hidden["pane"], hidden["id"], self.args.remote_cwd)
        frame("Claude needs permission")
        detected("claude", "blocked")
        frame()
        wait_for(lambda: agent() is None, "non-agent frame clears hidden SSH agent")
        frame("Codex working")
        detected("codex", "working")
        self.ssh(path, "disconnect", workspace=workspace)
        wait_for(lambda: agent() is None, "disconnect clears SSH screen identity")
        # Old terminal buffers remain after disconnect; later periodic scans must
        # not republish them as a running agent.
        for _ in range(20):
            assert agent() is None, "Disconnected terminal buffer revived an agent"
            time.sleep(.1)
        self.pass_check("hidden SSH frames update and clear agents; disconnect prevents stale-buffer rediscovery")

        frame()
        self.ssh(path, "connect", workspace=workspace)
        self.connected(path, workspace)
        assert agent() is None, "Reconnect retained the previous channel's identity"
        start()
        frame("Codex working")
        detected("codex", "working")
        frame(exit=True)
        wait_for(lambda: "exited" in self.ssh(path, "status", workspace=workspace)["tabs"].get(surface, ""),
                 "fake agent terminal exits")
        wait_for(lambda: agent() is None, "channel exit clears SSH screen identity")
        self.pass_check("reconnected SSH channel detects agents again and terminal exit removes them")

    def run(self):
        self.start_display()
        first, path = self.window("window-a")
        workspace = self.create(path)
        initial = self.workspace(path, workspace)
        pane, tab = self.terminals(initial)[0]
        original_tab = tab["id"]
        self.marker(path, pane, original_tab, self.args.remote_cwd)
        self.check_hangul(path, pane)
        self.rpc(path, "surface_create", workspace=workspace, cwd=None)
        split = self.rpc(path, "pane_split", pane=pane, direction="vertical")["pane_split_done"]["new_pane"]
        self.rpc(path, "pane_split", pane=split, direction="horizontal")
        ws = self.workspace(path, workspace)
        assert len(ws["panes"]) == 3 and len(self.terminals(ws)) == 4
        pids = {(p, t["id"]): self.marker(path, p, t["id"], self.args.remote_cwd) for p, t in self.terminals(ws)}
        assert len(set(pids.values())) == 4
        self.pass_check("new tab and two splits run four distinct remote shells; local socket environment absent")

        changed = self.args.remote_cwd + "/gui-" + uuid.uuid4().hex
        self.send(path, split, f"mkdir -p {shlex.quote(changed)}; cd {shlex.quote(changed)}; printf '\\033]7;file://fixture%s\\007' \"$PWD\"")
        wait_for(lambda: changed in self.state_path.read_text(), "remote OSC7 persisted")
        self.rpc(path, "pane_focus", pane=split)
        new_tab = self.rpc(path, "surface_create", workspace=workspace, cwd=None)["surface_created"]
        assert new_tab["pane"] == split
        self.marker(path, split, new_tab["id"], changed)
        self.pass_check("new terminal inherits remote OSC7 cwd, including spaces")

        forward = str(uuid.uuid4())
        status = self.ssh(path, "forward_add", workspace=workspace,
                          spec={"id": forward, "remote_port": self.args.http_port, "local_port": None, "https": False})
        port = next(item["local_port"] for item in status["forwards"] if item["id"] == forward)
        url = f"http://127.0.0.1:{port}"
        def http():
            with urllib.request.urlopen(url, timeout=3) as response:
                return response.read() == b"flowmux-remote-preview"
        assert http()
        self.rpc(path, "surface_close", pane=pane, surface=original_tab)
        self.connected(path, workspace)
        for (p, surface), pid in pids.items():
            if surface != original_tab:
                cwd = changed if p == split else self.args.remote_cwd
                assert self.marker(path, p, surface, cwd) == pid
        assert http()
        preview = self.ssh(path, "preview", workspace=workspace, id=forward)
        self.rpc(path, "surface_focus", pane=preview["pane"], surface=preview["surface"])
        wait_for(lambda: self.rpc(path, "browser_url", pane=preview["pane"])["browser_result"]["value"].startswith(url), "preview load")
        self.pass_check("closing first tab preserves sibling PIDs, master, and HTTP forwarding")
        self.check_removed_preview(path, workspace)

        self.ssh(path, "disconnect", workspace=workspace)
        assert self.ssh(path, "status", workspace=workspace)["state"] == "disconnected"
        with socket.socket() as probe:
            assert probe.connect_ex(("127.0.0.1", port)) != 0
        disconnected_tab = self.rpc(path, "surface_create", workspace=workspace, cwd=None)["surface_created"]
        try:
            self.send(path, disconnected_tab["pane"], "printf unexpected-local-shell")
        except RuntimeError:
            pass
        else:
            raise AssertionError("Disconnected terminal accepted input")
        assert all(t["kind"] in ("ssh_terminal", "browser") for p in self.workspace(path, workspace)["panes"] for t in p["tabs"])
        self.ssh(path, "connect", workspace=workspace)
        self.connected(path, workspace)
        self.marker(path, disconnected_tab["pane"], disconnected_tab["id"], self.args.remote_cwd)
        self.pass_check("disconnect removes forwarding; new disconnected tab fails closed and connects remotely")

        tmux_workspace = self.create(path, tmux=True)
        tmux_pane, tmux_tab = self.terminals(self.workspace(path, tmux_workspace))[0]
        session = "flowmux-" + tmux_tab["id"].replace("-", "")
        tmux_pid = self.marker(path, tmux_pane, tmux_tab["id"], self.args.remote_cwd)
        self.check_hangul(path, tmux_pane)
        self.ssh(path, "disconnect", workspace=tmux_workspace)
        self.remote(f"tmux has-session -t {shlex.quote(session)}")
        self.ssh(path, "connect", workspace=tmux_workspace)
        self.connected(path, tmux_workspace)
        assert self.marker(path, tmux_pane, tmux_tab["id"], self.args.remote_cwd) == tmux_pid
        self.check_hangul(path, tmux_pane)
        self.pass_check("Hangul input, character deletion, and output survive plain SSH and tmux reconnect")
        self.pass_check("tmux shell PID survives explicit disconnect/reconnect")

        second, second_path = self.window("window-b")
        before_a = {ws["id"] for ws in self.tree(path)}
        second_workspace = self.create(second_path)
        assert {ws["id"] for ws in self.tree(path)} == before_a
        assert second_workspace not in before_a
        cli_tree = subprocess.run([self.args.cli, "--socket", str(second_path), "--json", "tree"],
                                  env=self.env, text=True, capture_output=True, timeout=15, check=True)
        (self.root / "window-b-cli-tree.json").write_text(cli_tree.stdout)
        assert second_workspace in cli_tree.stdout and tmux_workspace not in cli_tree.stdout
        self.pass_check("two isolated windows route explicit IPC/CLI requests independently")

        before_restart = {ws["id"]: ws["panes"] for ws in self.tree(path)}
        for ws_id, panes in before_restart.items():
            self.persisted(ws_id, [tab["id"] for pane in panes for tab in pane["tabs"]])
        self.close_window(first)
        first, path = self.window("window-a-restored")
        restored = {ws["id"]: ws["panes"] for ws in self.tree(path)}
        assert set(restored) == set(before_restart)
        for ws_id in restored:
            assert [p["id"] for p in restored[ws_id]] == [p["id"] for p in before_restart[ws_id]]
            assert [[t["id"] for t in p["tabs"]] for p in restored[ws_id]] == [[t["id"] for t in p["tabs"]] for p in before_restart[ws_id]]
            assert self.ssh(path, "status", workspace=ws_id)["state"] == "disconnected"
        self.rpc(path, "surface_focus", pane=preview["pane"], surface=preview["surface"])
        try:
            inactive_url = self.rpc(path, "browser_url", pane=preview["pane"])["browser_result"]["value"]
            assert inactive_url in ("", "about:blank"), inactive_url
        except RuntimeError:
            pass  # A disconnected preview is a placeholder, without a WebView.
        self.ssh(path, "connect", workspace=tmux_workspace)
        self.connected(path, tmux_workspace)
        assert self.marker(path, tmux_pane, tmux_tab["id"], self.args.remote_cwd) == tmux_pid
        self.pass_check("own GUI restart restores disconnected layout and inactive preview; tmux process survives")

        self.ssh(path, "disconnect", workspace=tmux_workspace)
        self.remote(f"tmux kill-session -t {shlex.quote(session)}")
        self.ssh(path, "connect", workspace=tmux_workspace)
        self.connected(path, tmux_workspace)
        wait_for(lambda: "exited" in self.ssh(path, "status", workspace=tmux_workspace)["tabs"].get(tmux_tab["id"], ""), "missing tmux attach exits")
        assert self.remote(f"tmux has-session -t {shlex.quote(session)}", check=False).returncode != 0
        self.pass_check("missing tmux session is never recreated by reconnect")
        self.check_one_shot(path)
        self.check_agent_fallback(path)
        self.assert_protected()
        (self.root / "final-tree.json").write_text(json.dumps(self.tree(path), indent=2))
        self.log("complete", protected_pids=sorted(self.protected_pids), display=self.env["DISPLAY"])
        if self.args.keep:
            print(f"KEEP: DISPLAY={self.env['DISPLAY']} GUI PIDs={first.pid},{second.pid}; Ctrl+C/SIGTERM cleans up", flush=True)
            while True:
                signal.pause()
        self.close_window(first)
        self.close_window(second)

    def cleanup(self):
        for remote_file in self.remote_files:
            try:
                self.remote("rm -f -- " + shlex.quote(remote_file), check=False)
            except (OSError, RuntimeError, subprocess.TimeoutExpired):
                pass
        for session in self.sessions:
            try:
                self.remote(f"tmux kill-session -t {shlex.quote(session)}", check=False)
            except (OSError, RuntimeError, subprocess.TimeoutExpired):
                pass
        for process in reversed(self.children):
            self.stop(process)
        self.events.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--ssh-config", required=True)
    parser.add_argument("--host", default="flowmux-fixture")
    parser.add_argument("--remote-cwd", required=True)
    parser.add_argument("--http-port", type=int, required=True)
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("--protected-pid", type=int, action="append", default=[],
                        help="Additionally assert this existing process remains alive (repeatable)")
    args = parser.parse_args()
    for tool in ("Xvfb", "dbus-daemon", "ssh"):
        if not shutil.which(tool):
            parser.error(f"Required tool unavailable: {tool}")
    for attribute in ("gui", "cli", "ssh_config"):
        path = Path(getattr(args, attribute)).resolve(strict=True)
        setattr(args, attribute, str(path))
    harness = Harness(args)
    def interrupted(*_):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    try:
        harness.run()
    finally:
        harness.cleanup()


if __name__ == "__main__":
    main()
