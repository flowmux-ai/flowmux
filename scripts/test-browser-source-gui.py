#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify browser-open source routing with stale shared-Codex environment.

Uses isolated Xvfb, D-Bus and XDG state; never restarts the user's GUI.
"""

import argparse
import importlib.util
import json
from pathlib import Path
import shlex
import subprocess
import sys
import uuid

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    'gui', Path(__file__).with_name('test-ssh-workspace-gui.py'))
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


def topology(workspaces):
    return [(w['id'], [(p['id'], [(t['id'], t['kind'], t['active']) for t in p['tabs']])
                       for p in w['panes']]) for w in workspaces]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default='target/debug/flowmux')
    parser.add_argument('--cli', default='target/debug/flowmuxctl')
    parser.add_argument('--protected-pid', type=int, action='append', default=[])
    args = parser.parse_args()
    args.gui = str(Path(args.gui).resolve(strict=True))
    args.cli = str(Path(args.cli).resolve(strict=True))
    h = gui.Harness(args)
    for key in ('CODEX_THREAD_ID', 'CODEX_SESSION_ID', 'CLAUDECODE'):
        h.env.pop(key, None)
    h.env['CODEX_HOME'] = str(h.root / 'codex-home')
    config = h.root / 'config/flowmux'
    config.mkdir()
    (config / 'options.json').write_text(json.dumps(dict(
        default_shell='/bin/sh', auto_resume_agent_sessions=False)))
    page = h.root / 'review.html'
    page.write_text('<!doctype html><title>Source workspace review</title><h1>Correct workspace</h1>')
    fixture = h.root / 'codex'
    fixture.write_text('#!' + sys.executable + '\n' + r'''
import json, os, socket, sys
request = dict(id=1, kind='request', verb='agent_activity_update',
    pane=os.environ['FLOWMUX_PANE_ID'], surface=os.environ['FLOWMUX_SURFACE_ID'],
    agent='codex', activity='running', session_id=sys.argv[1], pid=os.getpid(), source='flowmux:hook')
with socket.socket(socket.AF_UNIX) as stream:
    stream.connect(os.environ['FLOWMUX_SOCKET_PATH'])
    stream.sendall((json.dumps(request) + '\n').encode())
    stream.makefile().readline()
print('Source fixture ready', flush=True)
sys.stdin.readline()
''')
    fixture.chmod(0o700)
    try:
        h.start_display()
        process, socket = h.window('browser-source')

        def workspace(path, name):
            ws = h.rpc(path, 'workspace_create', name=name, root=str(h.root))['workspace_created']['id']
            return ws, h.workspace(path, ws)['panes'][0]['id']

        source_ws, source = workspace(socket, 'Source')
        stale_ws, stale = workspace(socket, 'Stale environment')
        stale_browser = h.rpc(socket, 'browser_open', url='about:blank', target_pane=stale,
                              direction='vertical')['browser_pane_opened']['pane']
        session = str(uuid.uuid4())

        def bind(pane):
            h.send(socket, pane, shlex.join([str(fixture), session]))
            gui.wait_for(lambda: any(t.get('agent', {}).get('session_id') == session
                for w in h.tree(socket) for p in w['panes'] if p['id'] == pane for t in p['tabs']),
                'exact fixture session binding')

        bind(source)
        env = dict(h.env, CODEX_THREAD_ID=session, FLOWMUX_SOCKET_PATH=str(socket),
                   FLOWMUX_PANE_ID=stale, FLOWMUX_WORKSPACE_ID=stale_ws)

        def open_browser(environment, *extra):
            return subprocess.run([args.cli, '--json', 'browser', 'open', page.as_uri(), *extra],
                env=environment, text=True, capture_output=True, timeout=20)

        stale_before = topology([h.workspace(socket, stale_ws)])
        result = open_browser(env)
        assert result.returncode == 0, result.stderr
        opened = json.loads(result.stdout)['browser_pane_opened']
        browser = opened['pane']
        assert browser in [p['id'] for p in h.workspace(socket, source_ws)['panes']], result.stdout
        assert topology([h.workspace(socket, stale_ws)]) == stale_before
        assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == stale_ws
        gui.wait_for(lambda: h.rpc(socket, 'browser_title', pane=browser)['browser_result']['value']
                     == 'Source workspace review', 'review page loaded in source workspace')
        h.pass_check('Stale pane/workspace environment opens the review in the exact session workspace without stealing focus')

        fallback = dict(env, CODEX_SESSION_ID=session)
        fallback.pop('CODEX_THREAD_ID')
        result = open_browser(fallback)
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)['browser_pane_opened'] == dict(
            pane=browser, placement_strategy='reuse_right_sibling')
        assert topology([h.workspace(socket, stale_ws)]) == stale_before
        h.pass_check('CODEX_SESSION_ID fallback reuses only the source workspace browser')

        other_process, other_socket = h.window('other-window')
        _, other_pane = workspace(other_socket, 'Other window')
        other_before = topology(h.tree(other_socket))
        for hint in (other_socket, h.root / 'run/closed.sock'):
            result = open_browser(dict(env, FLOWMUX_SOCKET_PATH=str(hint), FLOWMUX_PANE_ID=other_pane))
            assert result.returncode == 0, result.stderr
            assert json.loads(result.stdout)['browser_pane_opened']['pane'] == browser
            assert topology(h.tree(other_socket)) == other_before
        h.pass_check('Stale live and closed window sockets recover the exact source window')

        before = topology(h.tree(socket))
        unknown = dict(env, CODEX_THREAD_ID=str(uuid.uuid4()))
        result = open_browser(unknown)
        assert result.returncode != 0 and 'No unique live Flowmux pane' in result.stderr, result
        assert topology(h.tree(socket)) == before
        result = open_browser(unknown, '--pane', stale)
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)['browser_pane_opened']['pane'] == stale_browser
        result = open_browser(dict(h.env, FLOWMUX_SOCKET_PATH=str(socket), FLOWMUX_PANE_ID=stale))
        assert result.returncode == 0, result.stderr
        assert json.loads(result.stdout)['browser_pane_opened']['pane'] == stale_browser
        h.pass_check('Unknown sessions fail without mutation; explicit --pane and non-Codex environment routing remain usable')

        bind(stale)
        before = topology(h.tree(socket))
        result = open_browser(env)
        assert result.returncode != 0 and 'No unique live Flowmux pane' in result.stderr, result
        assert topology(h.tree(socket)) == before
        h.pass_check('Ambiguous session bindings cannot open a tab in either workspace')
        h.rpc(socket, 'workspace_focus', workspace=source_ws)
        ready = subprocess.run([args.cli, 'browser', 'wait', browser, '--text', 'Correct workspace'],
            env=dict(h.env, FLOWMUX_SOCKET_PATH=str(socket)), text=True, capture_output=True, timeout=20)
        assert ready.returncode == 0 and ready.stdout.strip() == 'true', ready
        capture = subprocess.run([args.cli, 'browser', 'screenshot', browser, str(h.root / 'source-browser.png')],
            env=dict(h.env, FLOWMUX_SOCKET_PATH=str(socket)), text=True, capture_output=True, timeout=20)
        assert capture.returncode == 0, capture.stderr
        h.close_window(other_process)
        h.close_window(process)
    finally:
        h.cleanup()


if __name__ == '__main__':
    main()
