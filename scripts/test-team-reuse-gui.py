#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Keep sequential Team assignments bounded while preserving live user panes."""
import argparse
from contextlib import closing
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

from Xlib import X, XK, display
from Xlib.ext import xtest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('recovery', Path(__file__).with_name('test-team-recovery-gui.py'))
recovery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recovery)
fixture = recovery.fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default=str(recovery.REPO / 'target/debug/flowmux'))
    parser.add_argument('--cli', default=str(recovery.REPO / 'target/debug/flowmuxctl'))
    parser.add_argument('--protected-pid', type=int, action='append', default=[])
    args = parser.parse_args()
    h = fixture.Harness(args)
    for key in list(h.env):
        if key.startswith('CODEX_'):
            h.env.pop(key)
    h.env.update(CODEX_HOME=str(h.root / 'codex'), CLAUDE_CONFIG_DIR=str(h.root / 'claude'),
                 GEMINI_CLI_HOME=str(h.root / 'gemini'))
    native = h.root / 'providers'
    native.mkdir()
    for agent in ('codex', 'claude', 'gemini', 'agy'):
        path = native / agent
        path.write_text('#!' + sys.executable + '\n' + recovery.PROVIDER)
        path.chmod(0o700)
    h.env['PATH'] = str(native) + os.pathsep + str(Path(args.cli).parent) + os.pathsep + os.defpath
    (h.root / 'behavior.json').write_text('{}')
    task = h.root / 'task.txt'
    task.write_text('Read-only reuse fixture.')
    try:
        h.start_display()
        process, socket = h.window('reuse')
        workspace = h.rpc(socket, 'workspace_create', name='Team', root=str(h.root), team=True)['workspace_created']['id']
        source = h.workspace(socket, workspace)['panes'][0]['id']
        foreground = h.rpc(socket, 'workspace_create', name='Foreground', root=str(h.root))['workspace_created']['id']
        foreground_pane = h.workspace(socket, foreground)['panes'][0]['id']
        h.rpc(socket, 'workspace_focus', workspace=foreground)
        h.rpc(socket, 'pane_focus', pane=foreground_pane)
        env = dict(h.env, FLOWMUX_SOCKET_PATH=str(socket), FLOWMUX_PANE_ID=source)

        def command(*argv):
            result = subprocess.run([sys.executable, str(recovery.HELPER), *argv], env=env,
                                    capture_output=True, text=True, timeout=35)
            assert result.returncode == 0, result.stdout + result.stderr
            return json.loads(result.stdout)

        def start(agent='codex', *extra):
            return command('start', '--agent', agent, '--complexity', 'standard', '--role', 'reuse fixture',
                           '--cwd', str(h.root), '--task-file', str(task), '--cli', args.cli,
                           '--stall-timeout', '10', *extra)

        def finish(receipt):
            path = Path(receipt['job']) / 'status.json'
            state = fixture.wait_for(lambda: (s if (s := json.loads(path.read_text()))['state'] == 'completed' else None),
                                     'assignment completes', timeout=35)
            if state['agent'] in ('codex', 'claude'):
                h.rpc(socket, 'agent_activity_update', pane=state['pane'], surface=state['surface'],
                      agent=state['agent'], activity='idle', session_id=state['session_id'], source='flowmux:hook')
            return state

        def panes():
            return [p['id'] for p in h.workspace(socket, workspace)['panes']]

        def stopped(pid):
            stat = Path(f'/proc/{pid}/stat')
            return not stat.exists() or stat.read_text().split(') ', 1)[1].startswith('Z')

        with closing(display.Display(h.env['DISPLAY'])) as connection:
            atom = connection.intern_atom('_NET_WM_PID')
            window = next(w for w in connection.screen().root.query_tree().children
                          if (owner := w.get_full_property(atom, X.AnyPropertyType)) is not None
                          and int(owner.value[0]) == process.pid and w.get_geometry().width > 100)
            window.set_input_focus(X.RevertToParent, X.CurrentTime)
            connection.sync()
            time.sleep(.3)
            focus = connection.get_input_focus().focus

            def unchanged_focus():
                assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == foreground
                assert connection.get_input_focus().focus == focus

            prior = None
            for agent in ('codex', 'claude', 'gemini', 'agy'):
                receipt = start(agent)
                state = finish(receipt)
                if prior:
                    assert prior['pane'] not in panes()
                    fixture.wait_for(lambda: stopped(prior['pid']), 'previous provider stopped')
                original = (Path(receipt['job']) / 'result.md').read_text()
                for index in range(2):
                    following = start(agent, '--role', f'reused role {index}')
                    next_state = finish(following)
                    assert following['reused'] and following['agent'] == agent
                    assert following['model'] == receipt['model'] and following['source_pane'] == source
                    assert following['pane'] == receipt['pane'], (receipt, following)
                    assert following['job'] != receipt['job']
                    assert next_state['session_id'] == state['session_id']
                    assert (Path(receipt['job']) / 'result.md').read_text() == original
                    assert set(panes()) == {source, receipt['pane']}
                    unchanged_focus()
                h.pass_check(f'{agent}: repeated starts reuse one pane and conversation with preserved reports and YOLO')
                prior = next_state

            receipt = start()
            state = finish(receipt)
            assert prior['pane'] not in panes() and set(panes()) == {source, receipt['pane']}
            h.pass_check('Sequential provider changes retire both interactive and completed stream workers before replacement')
            replacement = start('codex', '--complexity', 'complex')
            replacement_state = finish(replacement)
            assert replacement['pane'] != receipt['pane']
            assert set(panes()) == {source, replacement['pane']}
            fixture.wait_for(lambda: stopped(state['pid']), 'old provider stopped before replacement')
            assert (Path(receipt['job']) / 'result.md').exists()
            unchanged_focus()
            h.pass_check('A different model retires its completed owned worker; pane count stays bounded and reports remain')
            editable = start('codex', '--complexity', 'complex', '--allow-edits')
            finish(editable)
            assert set(panes()) == {source, editable['pane']} and editable['pane'] != replacement['pane']
            fixture.wait_for(lambda: stopped(replacement_state['pid']), 'read-only worker stopped before authorized edit worker')
            unchanged_focus()
            h.pass_check('Changing edit authorization creates a replacement instead of changing the old conversation policy')
            h.rpc(socket, 'pane_close', pane=editable['pane'])

            for activity in ('running', 'needs_input'):
                receipt = start()
                state = finish(receipt)
                h.rpc(socket, 'agent_activity_update', pane=state['pane'], surface=state['surface'], agent='codex',
                      activity=activity, session_id=state['session_id'], source='flowmux:hook')
                separate = start()
                finish(separate)
                assert separate['pane'] != receipt['pane'] and receipt['pane'] in panes()
                assert not stopped(state['pid'])
                unchanged_focus()
                h.pass_check(f'{activity}: existing worker is neither reused nor closed')
                for pane in (receipt['pane'], separate['pane']):
                    h.rpc(socket, 'pane_close', pane=pane)

            receipt = start()
            state = finish(receipt)
            h.rpc(socket, 'agent_activity_update', pane=state['pane'], surface=state['surface'], agent='codex',
                  activity=None, session_id=state['session_id'], source='flowmux:hook')
            h.rpc(socket, 'agent_activity_update', pane=state['pane'], surface=state['surface'], agent='codex',
                  activity='idle', session_id='different-user-session', source='flowmux:hook')
            fixture.wait_for(lambda: any((t.get('agent') or {}).get('session_id') == 'different-user-session'
                                        for p in h.workspace(socket, workspace)['panes'] if p['id'] == state['pane']
                                        for t in p['tabs']), 'new native session binding is visible')
            separate = start()
            finish(separate)
            assert separate['pane'] != receipt['pane'] and receipt['pane'] in panes()
            assert not stopped(state['pid'])
            unchanged_focus()
            h.pass_check('A changed session is preserved while the new assignment receives its own worker')
            for pane in (receipt['pane'], separate['pane']):
                h.rpc(socket, 'pane_close', pane=pane)

            receipt = start()
            state = finish(receipt)
            h.rpc(socket, 'pane_focus', pane=receipt['pane'])
            extra = h.rpc(socket, 'surface_create', workspace=workspace, cwd=str(h.root))['surface_created']
            assert extra['pane'] == receipt['pane']
            h.rpc(socket, 'pane_focus', pane=foreground_pane)
            separate = start()
            finish(separate)
            old_pane = next(p for p in h.workspace(socket, workspace)['panes'] if p['id'] == receipt['pane'])
            assert len(old_pane['tabs']) == 2 and extra['id'] in [t['id'] for t in old_pane['tabs']]
            assert separate['pane'] != receipt['pane'] and not stopped(state['pid'])
            unchanged_focus()
            h.pass_check('An extra user terminal tab excludes its whole pane from reuse and retirement')
            for pane in (receipt['pane'], separate['pane']):
                h.rpc(socket, 'pane_close', pane=pane)

            receipt = start()
            finish(receipt)
            (h.root / 'behavior.json').write_text(json.dumps({'codex': 'progress'}))
            argv = [sys.executable, str(recovery.HELPER), 'start', '--agent', 'codex',
                    '--complexity', 'standard', '--role', 'concurrent fixture', '--cwd', str(h.root),
                    '--task-file', str(task), '--cli', args.cli, '--stall-timeout', '10']
            callers = [subprocess.Popen(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                       for _ in range(2)]
            receipts = []
            for caller in callers:
                stdout, stderr = caller.communicate(timeout=35)
                assert caller.returncode == 0, stdout + stderr
                receipts.append(json.loads(stdout))
            for item in receipts:
                finish(item)
            assert len({item['job'] for item in receipts}) == len({item['pane'] for item in receipts}) == 2
            assert receipt['pane'] in {item['pane'] for item in receipts} and len(panes()) == 3
            unchanged_focus()
            h.pass_check('Concurrent starts never dispatch two assignments into the same idle worker')
            for item in receipts:
                h.rpc(socket, 'pane_close', pane=item['pane'])

            for character in 'reusekeepsinput':
                code = connection.keysym_to_keycode(XK.string_to_keysym(character))
                xtest.fake_input(connection, X.KeyPress, code)
                xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()
            fixture.wait_for(lambda: 'reusekeepsinput' in h.screen(socket, foreground_pane), 'native input remains in foreground')
            h.pass_check('Native keyboard input stays in the foreground workspace throughout reuse and replacement')
    finally:
        h.cleanup()


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))
    main()
