#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify visible and hidden terminal viewport reads in an isolated GUI."""

import argparse
from contextlib import closing
import importlib.util
import json
from pathlib import Path
import shlex
import signal
import sys
import time

from Xlib import X, display

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "fixture", Path(__file__).with_name("test-ssh-workspace-gui.py"))
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class Harness(fixture.Harness):
    def spawn(self, argv, **kwargs):
        return super().spawn([arg.replace("1600x1000x24", "1600x2400x24")
                              for arg in argv], **kwargs)

    def run(self):
        self.env.update(CODEX_HOME=str(self.root / "codex"),
                        CLAUDE_CONFIG_DIR=str(self.root / "claude"))
        config = self.root / "config/flowmux"
        config.mkdir()
        (config / "options.json").write_text(json.dumps({
            "default_shell": "/bin/sh", "usage_bar_enabled": False,
            "terminal_minimap_enabled": False}))
        probe = self.root / "probe.py"
        probe.write_text("""import json, os, sys
from pathlib import Path
columns, rows = os.get_terminal_size(1)
if sys.argv[2] == 'alternate':
    sys.stdout.write('\\x1b[?1049h')
sys.stdout.write('\\x1b[2J\\x1b[H')
for row in range(1, rows + 1):
    label = f'ROW_{row:03}_'
    sys.stdout.write(f'\\x1b[{row};1H' + label + '.' * (columns - len(label) - 1) + 'X')
sys.stdout.write('\\x1b[H')
sys.stdout.flush()
Path(sys.argv[1]).write_text(json.dumps([rows, columns]))
sys.stdin.readline()
""")
        self.start_display()
        process, socket = self.window("read-screen")
        results = []
        failures = []
        with closing(display.Display(self.env["DISPLAY"])) as connection:
            def own_window():
                for window in connection.screen().root.query_tree().children:
                    pid = window.get_full_property(connection.intern_atom("_NET_WM_PID"),
                                                   X.AnyPropertyType)
                    if (pid is not None and int(pid.value[0]) == process.pid
                            and window.get_attributes().map_state == X.IsViewable):
                        return window
                return None

            window = fixture.wait_for(own_window, "isolated GUI window")
            window.set_input_focus(X.RevertToParent, X.CurrentTime)
            connection.sync()
            for height in (700, 1900):
                window.configure(width=1100, height=height)
                connection.sync()
                for mode in ("normal", "alternate"):
                    workspace = self.rpc(socket, "workspace_create", name=f"{height}-{mode}",
                                         root=str(self.root))["workspace_created"]["id"]
                    pane = self.workspace(socket, workspace)["panes"][0]["id"]
                    self.rpc(socket, "workspace_focus", workspace=workspace)
                    time.sleep(.5)
                    ready = self.root / f"ready-{height}-{mode}.json"
                    self.send(socket, pane, shlex.join([sys.executable, str(probe), str(ready), mode]))
                    fixture.wait_for(ready.exists, "viewport painted")
                    rows, columns = json.loads(ready.read_text())
                    assert (rows > 80) == (height == 1900), (height, rows)
                    expected = [f"ROW_{row:03}_" + "." * (columns - 9) + "X"
                                for row in range(1, rows + 1)]
                    fixture.wait_for(lambda: expected[-1] in self.screen(socket, pane),
                                     "visible final cell")
                    shown = self.screen(socket, pane)
                    foreground = self.rpc(socket, "workspace_create", name="Foreground",
                                          root=str(self.root))["workspace_created"]["id"]
                    self.rpc(socket, "workspace_focus", workspace=foreground)
                    # Allow the workspace stack's unmap transition to finish.
                    time.sleep(.75)
                    focus = connection.get_input_focus().focus
                    hidden = self.screen(socket, pane)
                    assert self.rpc(socket, "workspace_current")["workspace_current"]["id"] == foreground
                    assert connection.get_input_focus().focus == focus, "read-screen changed X focus"
                    self.rpc(socket, "workspace_focus", workspace=workspace)
                    time.sleep(.2)
                    restored = self.screen(socket, pane)
                    result = dict(height=height, mode=mode, rows=rows, columns=columns,
                                  shown=shown, hidden=hidden, restored=restored)
                    results.append(result)
                    for state in ("shown", "hidden", "restored"):
                        missing = [row + 1 for row, line in enumerate(expected)
                                   if line not in result[state]]
                        if missing:
                            failures.append(f"{height}/{mode}/{state}: missing rows/cells {missing}")
                    self.pass_check(f"{height}/{mode}: background read preserves workspace and X focus")
        (self.root / "result.json").write_text(json.dumps(results, indent=2))
        self.close_window(process)
        assert not failures, "\n".join(failures)
        self.pass_check("normal and alternate viewports retain every row and final column, including >80 rows")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--gui", default="target/debug/flowmux")
    parser.add_argument("--cli", default="target/debug/flowmuxctl")
    parser.add_argument("--protected-pid", type=int, action="append", default=[])
    args = parser.parse_args()
    for attribute in ("gui", "cli"):
        setattr(args, attribute, str(Path(getattr(args, attribute)).resolve(strict=True)))
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
