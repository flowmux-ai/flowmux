#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check Codex title-job routing and missing start hooks in a disposable GUI.

Uses the shared Linux Xvfb harness, AT-SPI, Pillow and a small C terminal fixture.
No real agent sessions or hook trust settings are changed.
"""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import time

from PIL import Image
from Xlib import X, display

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "gui", Path(__file__).with_name("test-ssh-workspace-gui.py"))
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    args.gui = str(Path(args.gui).resolve(strict=True))
    args.cli = str(Path(args.cli).resolve(strict=True))
    h = gui.Harness(args)
    connection = None
    try:
        h.env["GTK_A11Y"] = "atspi"
        h.env["CODEX_HOME"] = str(h.root / "codex-home")
        history = Path(h.env["CODEX_HOME"]) / "sessions/2026/10/10"
        history.mkdir(parents=True)
        shell = h.root / "clean-shell"
        shell.write_text("#!/bin/sh\nexec /bin/bash --noprofile --norc\n")
        shell.chmod(0o755)
        config = h.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({"default_shell": str(shell)}))
        source = h.root / "agent.c"
        source.write_text(r'''#include <stdio.h>
int main(void) {
    char line[64];
    while (fgets(line, sizeof(line), stdin)) {
        printf("\033[2J\033[H");
        if (line[0] != 'i')
            printf("• Waiting for background terminal (%cs • esc to interrupt)\n", line[0]);
        else
            printf("?? .claude/\n• Done.\n");
        printf("› Ask Codex to do anything\nGPT-6-Astra · ~/work\n");
        fflush(stdout);
    }
}
''')
        executable = h.root / "codex"
        subprocess.run(["cc", str(source), "-o", str(executable)], check=True, timeout=30)
        h.start_display()
        os.environ["DBUS_SESSION_BUS_ADDRESS"] = h.env["DBUS_SESSION_BUS_ADDRESS"]
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi
        Atspi.set_timeout(3000, 3000)
        connection = display.Display(h.env["DISPLAY"])
        process, socket = h.window("codex-status")
        created = h.rpc(socket, "workspace_create", name="Codex status", root=str(h.root))
        ws = h.workspace(socket, created["workspace_created"]["id"])
        pane = ws["panes"][0]["id"]
        surface = ws["panes"][0]["tabs"][0]["id"]
        h.send(socket, pane, "exec " + shlex.quote(str(executable)))

        def observed():
            return h.workspace(socket, ws["id"])["panes"][0]["tabs"][0].get("agent")

        def status(expected):
            return gui.wait_for(lambda: (value := observed())
                                and value["status"] in expected and value,
                                f"agent status {expected}", timeout=8)

        def screen(command):
            h.rpc(socket, "pane_send_keys", pane=pane, keys=command + "\r")

        root_session = "33333333-3333-4333-8333-333333333333"
        hook_env = dict(h.env, FLOWMUX_RUNTIME_DIR=str(socket.parent))

        def hook(session, event, **fields):
            subprocess.run(["/bin/sh", "-c", '"$@"; result=$?; exit $result',
                            "--managed-daemon", args.cli, "--socket", str(socket),
                            "hooks", "codex", event],
                           input=json.dumps(dict(session_id=session, cwd=str(h.root), **fields)),
                           env=hook_env, text=True, capture_output=True, check=True, timeout=10)

        status(("idle",))
        screen("1")
        status(("working",))
        # The title job has a cwd but no persisted root session. Its Stop must
        # neither claim this unnamed Codex tab nor publish a completion.
        hook("11111111-1111-4111-8111-111111111111", "stop", turn_id="title-turn",
             last_assistant_message='{"title":"Task title"}')
        time.sleep(.6)
        assert observed()["status"] == "working", observed()
        assert observed().get("session_id") is None, observed()
        h.pass_check("ephemeral title completion cannot claim a Codex pane")

        # Subagent histories must not claim a root pane either.
        for session, source_kind in (("22222222-2222-4222-8222-222222222222", {"subagent": {}}), (root_session, "cli")):
            (history / f"rollout-{session}.jsonl").write_text(json.dumps(dict(
                type="session_meta", payload=dict(id=session, cwd=str(h.root), source=source_kind))) + "\n")
        hook("22222222-2222-4222-8222-222222222222", "stop", turn_id="child-turn")
        time.sleep(.6)
        assert observed().get("session_id") is None, observed()
        h.pass_check("subagent completion cannot claim a root pane")

        hook(root_session, "stop", turn_id="legacy-first")
        gui.wait_for(lambda: observed().get("session_id") == root_session, "persisted root route")
        status(("idle", "done"))
        screen("1")
        time.sleep(.5)
        assert observed()["status"] in ("idle", "done"), observed()
        away = h.rpc(socket, "workspace_create", name="Other workspace", root=str(h.root))
        for visible, native in ((False, False), (False, True), (True, False), (True, True)):
            focused = ws["id"] if visible else away["workspace_created"]["id"]
            h.rpc(socket, "workspace_focus", workspace=focused)
            turn = f"turn-{visible}-{native}"
            if native:
                hook(root_session, "turn-start", turn_id=turn)
            screen("2")
            gui.wait_for(lambda: "(2s" in (observed().get("custom_status") or ""), "live progress")
            hook(root_session, "stop", turn_id=turn)
            status(("idle", "done"))
            screen("2")
            time.sleep(.5)
            assert observed()["status"] in ("idle", "done"), observed()
            # No TurnStarted: legacy notify and missing native start hooks.
            screen("3")
            status(("working",))
            screen("idle")
            status(("idle", "done"))
            screen("4")
            status(("working",))
            assert h.rpc(socket, "workspace_current")["workspace_current"]["id"] == focused
            h.pass_check(f"visible={visible}, native={native}: stale repaint ignored; missing start recovers and settles")

        def progress_label():
            desktop = Atspi.get_desktop(0)
            pending = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
            pending = [node for node in pending if node.get_process_id() == process.pid]
            while pending:
                node = pending.pop()
                if node is None:
                    continue
                if (node.get_role() == Atspi.Role.LABEL
                        and "Waiting for background terminal (4s" in node.get_name()
                        and node.get_state_set().contains(Atspi.StateType.SHOWING)):
                    return node.get_name()
                pending.extend(node.get_child_at_index(i) for i in range(node.get_child_count()))
            return None

        label = gui.wait_for(progress_label, "visible Agents progress label")
        h.log("agents_label", text=label)
        root = connection.screen().root
        geometry = root.get_geometry()
        raw = root.get_image(0, 0, geometry.width, geometry.height, X.ZPixmap, 0xffffffff)
        Image.frombytes("RGB", (geometry.width, geometry.height), raw.data,
                        "raw", "BGRX").save(h.root / "agents-working.png")
        h.pass_check("Agents visibly shows recovered Codex progress")
        h.close_window(process)
    finally:
        if connection:
            connection.close()
        h.cleanup()


if __name__ == "__main__":
    main()
