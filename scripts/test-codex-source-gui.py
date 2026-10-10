#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify a managed Codex lead's first tool without pre-seeding session hooks.

Run through the ignored codex_shim_first_tool_gui Rust test, which supplies the
actual generated shim. Uses isolated Xvfb, D-Bus and XDG state; no user restart.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('gui', REPO / 'scripts/test-ssh-workspace-gui.py')
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default=str(REPO / 'target/debug/flowmux'))
    parser.add_argument('--cli', default=str(REPO / 'target/debug/flowmuxctl'))
    parser.add_argument('--shim', type=Path, required=True)
    parser.add_argument('--protected-pid', type=int, action='append', default=[])
    args = parser.parse_args()
    h = gui.Harness(args)
    for key in ('CODEX_THREAD_ID', 'CODEX_SESSION_ID', 'CLAUDECODE'):
        h.env.pop(key, None)
    options = h.root / 'config/flowmux/options.json'
    options.parent.mkdir()
    options.write_text(json.dumps(dict(default_shell='/bin/sh', auto_resume_agent_sessions=False)))
    shim = h.root / 'data/flowmux/shims/codex'
    shim.parent.mkdir(parents=True)
    stale_shim = '#!/bin/sh\n# flowmux agent wrapper shim\necho stale-wrapper >&2\nexit 70\n'
    shim.write_text(stale_shim)
    shim.chmod(0o700)
    test_home = h.root / 'home'
    legacy = test_home / '.local/bin/codex'
    legacy.parent.mkdir(parents=True)
    legacy.write_text(stale_shim)
    legacy.chmod(0o700)
    # The installer must upgrade both PATH locations without provider config edits.
    refresh = subprocess.run([args.cli, 'hooks', 'refresh-shims'],
        env=dict(h.env, HOME=str(test_home)), capture_output=True, text=True)
    assert refresh.returncode == 0, refresh.stderr
    assert shim.read_bytes() == legacy.read_bytes() == args.shim.read_bytes()
    assert sorted(p.name for p in test_home.iterdir()) == ['.local']
    h.pass_check('Installer refresh upgrades existing wrappers without creating provider settings')
    bin_dir = h.root / 'bin'
    bin_dir.mkdir()
    provider = bin_dir / 'codex'
    provider.write_text('#!/usr/bin/env python3\n' + r'''
import json, os, pathlib, subprocess, sys
if sys.argv[1:] == ['--help']:
    print('--no-daemon')
    raise SystemExit()
root = pathlib.Path(os.environ['XDG_STATE_HOME']).parent
pane = os.environ['FLOWMUX_PANE_ID']
(root / (pane + '-argv.json')).write_text(json.dumps(sys.argv))
print('Codex lead ready, no lifecycle hook', flush=True)
for line in sys.stdin:
    env = dict(os.environ, CODEX_THREAD_ID='unbound-' + pane)
    if '--no-daemon' not in sys.argv:
        env['FLOWMUX_SOCKET_PATH'] = str(root / 'closed.sock')
    result = subprocess.run(json.loads(line), env=env, capture_output=True, text=True)
    (root / (pane + '-context.json')).write_text(json.dumps(dict(
        code=result.returncode, stdout=result.stdout, stderr=result.stderr)))
    print(result.stdout or result.stderr, flush=True)
''')
    provider.chmod(0o700)
    h.env['PATH'] = str(bin_dir) + os.pathsep + h.env['PATH']
    helper = REPO / '.agents/skills/flowmux-team/scripts/team.py'
    try:
        h.start_display()
        old, stale_socket = h.window('closed-source')
        h.rpc(stale_socket, 'workspace_create', name='Old window', root=str(h.root))
        h.close_window(old)
        assert not stale_socket.exists()
        process, socket = h.window('new-leads')
        leads = []
        for label in ('first', 'second'):
            ws_id = h.rpc(socket, 'workspace_create', name='Same project', root=str(h.root),
                          team=True)['workspace_created']['id']
            pane = h.workspace(socket, ws_id)['panes'][0]['id']
            # Resolve via the installed shim as a user typing `codex` would.
            h.send(socket, pane, 'codex')
            argv_file = h.root / (pane + '-argv.json')
            gui.wait_for(argv_file.exists, 'lead ' + label)
            assert json.loads(argv_file.read_text())[1:] == ['--no-daemon']
            leads.append((ws_id, pane))
        # Both have the same cwd/title and neither has a reported session.
        for ws_id, pane in leads:
            tab = h.workspace(socket, ws_id)['panes'][0]['tabs'][0]
            assert not tab.get('agent', {}).get('session_id'), tab
            other = next(ws for ws, p in leads if p != pane)
            h.rpc(socket, 'workspace_focus', workspace=other)
            command = [sys.executable, str(helper), 'context', '--cli', args.cli]
            h.rpc(socket, 'pane_send_keys', pane=pane, keys=json.dumps(command) + '\r')
            result_file = h.root / (pane + '-context.json')
            gui.wait_for(result_file.exists, 'first tool context')
            result = json.loads(result_file.read_text())
            assert result['code'] == 0, result
            assert json.loads(result['stdout']) == dict(socket=str(socket), pane=pane,
                workspace=ws_id, workspace_type='team', mode='team'), result
            assert not h.workspace(socket, ws_id)['panes'][0]['tabs'][0].get('agent', {}).get('session_id')
            (h.root / (pane + '-screen.txt')).write_text(h.screen(socket, pane))
        h.pass_check('Two same-cwd unbound Codex leads resolve their first tool by process ancestry, independent of focus and hooks')
        result_file.unlink()
        shared = ['/bin/sh', '-c', '"$@"; result=$?; exit "$result"', '--managed-daemon', *command]
        h.rpc(socket, 'pane_send_keys', pane=pane, keys=json.dumps(shared) + '\r')
        gui.wait_for(result_file.exists, 'unbound shared daemon rejection')
        rejected = json.loads(result_file.read_text())
        assert rejected['code'] != 0 and 'No unique live Flowmux pane' in rejected['stderr'], rejected
        (h.root / 'shared-daemon-rejected.json').write_text(json.dumps(rejected, indent=2))
        h.pass_check('A shared daemon cannot claim its ancestor pane without an exact session binding')
        assert not stale_socket.exists()
        h.pass_check('Closed socket stays absent; no shared-daemon or session binding was manufactured')
        surfaces = {
            h.workspace(socket, ws)['panes'][0]['tabs'][0]['id']: 'fixture-' + pane
            for ws, pane in leads
        }
        h.close_window(process)
        for _, pane in leads:
            (h.root / (pane + '-argv.json')).unlink()
            (h.root / (pane + '-context.json')).unlink()
        # Simulate login startup prepending the provider's directory before -c.
        shell = h.root / 'login-shell'
        shell.write_text('#!/bin/sh\nexport PATH=' + shlex.quote(str(bin_dir) + ':/usr/bin:/bin') +
                         '\nexec /bin/sh "$@"\n')
        shell.chmod(0o700)
        options.write_text(json.dumps(dict(default_shell=str(shell), auto_resume_agent_sessions=True)))
        sessions = h.root / 'data/flowmux/agent-sessions/codex.json'
        sessions.parent.mkdir(parents=True, exist_ok=True)
        sessions.write_text(json.dumps(surfaces))
        _, socket = h.window('restored-leads')
        for ws_id, pane in leads:
            argv_file = h.root / (pane + '-argv.json')
            gui.wait_for(argv_file.exists, 'restored local lead')
            assert json.loads(argv_file.read_text())[1:] == ['--no-daemon', 'resume', 'fixture-' + pane]
            h.rpc(socket, 'pane_send_keys', pane=pane, keys=json.dumps(command) + '\r')
            result_file = h.root / (pane + '-context.json')
            gui.wait_for(result_file.exists, 'restored first tool')
            result = json.loads(result_file.read_text())
            assert result['code'] == 0, result
            assert json.loads(result['stdout'])['workspace'] == ws_id, result
        h.pass_check('Auto-resumed leads retain local source ownership after login startup reorders PATH')
    finally:
        h.cleanup()


if __name__ == '__main__':
    main()
