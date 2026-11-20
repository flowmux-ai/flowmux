#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""A terminal's shell runs in the Agent Host: an agent typed into it outlives the window."""

import argparse
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys
import time
from types import SimpleNamespace

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("gui_fixture", ROOT / "scripts/test-ssh-workspace-gui.py")
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("gui", "cli", "host"):
        default = {"gui": "flowmux", "cli": "flowmuxctl", "host": "flowmux-daemon"}[name]
        parser.add_argument(f"--{name}", default=str(ROOT / "target/debug" / default))
    parser.add_argument("--protected-pid", action="append", type=int, default=[])
    args = parser.parse_args()
    for name in ("gui", "cli", "host"):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = gui.Harness(SimpleNamespace(gui=args.gui, cli=args.cli, protected_pid=args.protected_pid,
                                    hosted_terminals=True))
    host_socket = h.root / "run/flowmux-agent-host.sock"

    def hosted():
        if not h.state_path.exists():
            return []
        return re.findall(r'"shell_binding":\s*\{\s*"instance":\s*"([0-9a-f-]+)"', h.state_path.read_text())

    def agents():
        result = subprocess.run([args.cli, "--json", "agent", "ls"], env=h.env, capture_output=True,
                                text=True, timeout=20)
        assert result.returncode == 0, (result.stdout, result.stderr)
        return json.loads(result.stdout)

    try:
        h.start_display()
        config = h.root / "config/flowmux"
        config.mkdir(parents=True, exist_ok=True)
        (config / "options.json").write_text(json.dumps({"default_shell": "/bin/sh"}))
        with (h.root / "host.log").open("w") as log:
            h.spawn([args.host, "--agent-host", "--socket", str(host_socket)], stdout=log, stderr=log)
        gui.wait_for(host_socket.exists, "isolated Host")
        process, socket = h.window("hosted")
        workspace = h.rpc(socket, "workspace_create", name="Hosted", root=str(h.root))["workspace_created"]["id"]
        pane = h.workspace(socket, workspace)["panes"][0]["id"]
        first = h.workspace(socket, workspace)["panes"][0]["tabs"][0]["id"]
        gui.wait_for(lambda: len(hosted()) == 1, "the terminal's shell runs in the Host")
        h.send(socket, pane, "echo HOSTED_$((40+2))")
        gui.wait_for(lambda: "HOSTED_42" in h.screen(socket, pane), "hosted shell answers")
        h.send(socket, pane, "seq 1 2000; cd /usr && echo MOVED_$((1+1))")
        gui.wait_for(lambda: "MOVED_2" in h.screen(socket, pane), "long output and cd")
        gui.wait_for(lambda: '"cwd": "/usr"' in h.state_path.read_text(), "the tab follows its shell's directory")
        assert not agents(), "a shell without an agent is not an agent"
        h.pass_check("a terminal's shell runs in the Host, types, and reports its directory like a local one")

        h.rpc(socket, "surface_create", workspace=workspace, cwd=str(h.root))
        gui.wait_for(lambda: len(hosted()) == 2, "second hosted terminal")
        # The new tab stays a plain terminal and reports its shell.
        second = next(tab["id"] for tab in h.workspace(socket, workspace)["panes"][0]["tabs"] if tab["id"] != first)
        h.send(socket, pane, "echo $$ > plain.pid", tab=second)
        plain = h.root / "plain.pid"
        gui.wait_for(lambda: plain.exists() and plain.read_text().strip(), "plain terminal's shell")
        plain_pid = int(plain.read_text())
        fake = h.root / "claude"
        fake.write_text("import os, time\nprint('FAKE_AGENT', os.getpid(), flush=True)\nn = 0\n"
                        "while True:\n    n += 1\n    open('ticks', 'w').write(str(n))\n    time.sleep(.2)\n")
        h.rpc(socket, "surface_focus", pane=pane, surface=first)
        h.send(socket, pane, f"cd {h.root} && python3 ./claude", tab=first)
        ticks = h.root / "ticks"
        gui.wait_for(ticks.exists, "agent started in the terminal")
        kept = gui.wait_for(lambda: next((a for a in agents() if a.get("presence")), None),
                            "the Host finds the agent by its process")
        h.close_window(process)
        # The Host ends a shell nobody views ten seconds after its window, unless an agent runs in it.
        gui.wait_for(lambda: not Path(f"/proc/{plain_pid}").exists(),
                     "the plain terminal ends with its window", timeout=40)
        listed = agents()
        assert [a["instance"] for a in listed] == [kept["instance"]] and listed[0]["state"] == "running", listed
        before = int(ticks.read_text() or 0)
        gui.wait_for(lambda: int(ticks.read_text() or 0) > before, "the agent keeps working without a window")
        h.pass_check("closing the window ends the plain terminal and leaves the agent's terminal working")

        process, socket = h.window("reopened")
        gui.wait_for(lambda: "FAKE_AGENT" in h.screen(socket, pane), "restored tab shows the same agent")
        assert kept["instance"] in hosted()
        assert next(a for a in agents() if a["instance"] == kept["instance"])["pid"] == kept["pid"]
        h.pass_check("reopening connects the restored tab to the same process")
        h.close_window(process)
        assert next(a for a in agents() if a["instance"] == kept["instance"])["state"] == "running"

        # Answering the closing window's question with "stop" ends the agents and the Host.
        h.env["FLOWMUX_CLOSE_AGENTS"] = "stop"
        process, socket = h.window("stopping")
        gui.wait_for(lambda: "FAKE_AGENT" in h.screen(socket, pane), "the agent is shown again")
        # The window asks about the agents it has heard of; it polls the Host every second.
        time.sleep(3)
        h.close_window(process)
        gui.wait_for(lambda: not Path(f"/proc/{kept['pid']}").exists(), "the agent's terminal is stopped")
        gui.wait_for(lambda: not host_socket.exists(), "the Agent runtime exits with its last agent", timeout=40)
        h.pass_check("closing the window with \"stop\" ends the agents and the Agent runtime")
        print("HOSTED_TERMINAL_OK", flush=True)
    finally:
        if host_socket.exists():
            subprocess.run([args.cli, "agent", "stop-host"], env=h.env, capture_output=True, timeout=20)
        h.cleanup()


if __name__ == "__main__":
    main()
