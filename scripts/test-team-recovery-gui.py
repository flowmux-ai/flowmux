#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise managed provider recovery and follow-ups in isolated real Flowmux PTYs."""
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
REPO = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('fixture', REPO / 'scripts/test-ssh-workspace-gui.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)
HELPER = REPO / '.agents/skills/flowmux-team/scripts/team.py'

PROVIDER = r'''
import json, os, pathlib, re, signal, subprocess, sys, time
agent = pathlib.Path(sys.argv[0]).name
root = pathlib.Path(os.environ['XDG_STATE_HOME']).parent
prompt = sys.argv[-1]
token = re.search(r'FLOWMUX_TEAM_JOB:([^\s]+)', prompt)[1]
session = token
if '--resume' in sys.argv:
    session = sys.argv[sys.argv.index('--resume') + 1]
if '--conversation' in sys.argv:
    session = sys.argv[sys.argv.index('--conversation') + 1]
if agent == 'codex':
    assert '--no-daemon' in sys.argv and '--dangerously-bypass-approvals-and-sandbox' in sys.argv
elif agent == 'claude':
    assert '--allow-dangerously-skip-permissions' in sys.argv
    assert sys.argv[sys.argv.index('--permission-mode') + 1] == 'bypassPermissions'
elif agent == 'gemini':
    assert sys.argv[sys.argv.index('--approval-mode') + 1] == 'yolo'
else:
    assert '--dangerously-skip-permissions' in sys.argv
if agent == 'codex':
    transcript = pathlib.Path(os.environ['CODEX_HOME']) / 'sessions/2026/10/23' / (session + '.jsonl')
else:
    transcript = pathlib.Path(os.environ['CLAUDE_CONFIG_DIR']) / 'projects/fixture' / (session + '.jsonl')
transcript.parent.mkdir(parents=True, exist_ok=True)
def save(event):
    with transcript.open('a') as stream:
        stream.write(json.dumps(event) + '\n')
def emit(event):
    print(json.dumps(event), flush=True)
def run_tool():
    if failure not in ('quiet_tool', 'hung_tool', 'failed_tool'):
        return
    def tool(active):
        if agent == 'codex':
            save({'type': 'response_item', 'payload': {'type': 'function_call' if active else 'function_call_output',
                  'call_id': 'quiet', 'name': 'exec_command'}})
        elif agent == 'claude':
            block = {'type': 'tool_use', 'id': 'quiet', 'name': 'Bash'} if active else {'type': 'tool_result', 'tool_use_id': 'quiet', 'content': 'done'}
            save({'type': 'assistant' if active else 'user', 'message': {'content': [block]}})
        elif agent == 'gemini':
            emit({'type': 'tool_use' if active else 'tool_result', 'tool_id': 'quiet', 'tool_name': 'run_shell_command'})
        else:
            emit({'event': 'step_update', 'step_update': {'step_type': 'tool', 'step_index': 1, 'state': 'ACTIVE' if active else 'DONE'}})
    tool(True)
    if failure == 'failed_tool':
        emit({'event': 'step_update', 'step_update': {'step_type': 'tool', 'step_index': 1, 'state': 'ERROR'}})
        time.sleep(120)
    child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(' + ('120' if failure == 'hung_tool' else '6') + ')'])
    evidence['child'] = child.pid
    (root / (token + '-process.json')).write_text(json.dumps(evidence))
    child.wait()
    tool(False)
while True:
    token = re.search(r'FLOWMUX_TEAM_JOB:([^\s]+)', prompt)[1]
    behavior = json.loads((root / 'behavior.json').read_text())
    failure = behavior.get(agent)
    unavailable = {'model_unavailable': 'model_not_found', 'client_unavailable': 'unsupported_client'}.get(failure)
    evidence = {'argv': sys.argv, 'pid': os.getpid(), 'session': session, 'token': token, 'started': time.monotonic()}
    if failure in ('quota', 'stalled', 'crash', 'invalid_report', 'provider_error'):
        child = subprocess.Popen([sys.executable, '-c', 'import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);time.sleep(120)'])
        evidence['child'] = child.pid
    (root / (token + '-process.json')).write_text(json.dumps(evidence))
    answer = json.dumps({'task_status': 'completed', 'report': agent + ' verified'})
    if agent in ('codex', 'claude'):
        if agent == 'codex':
            save({'type': 'session_meta', 'payload': {'id': session}})
            save({'type': 'response_item', 'payload': {'role': 'user', 'content': prompt}})
        else:
            save({'type': 'user', 'sessionId': session, 'message': {'content': prompt}})
        print('WORKING ' + agent, flush=True)
        time.sleep(.6)
        run_tool()
        if failure == 'stalled':
            time.sleep(120)
        if failure == 'crash':
            os._exit(7)
        if failure in ('progress', 'long_progress'):
            for step in range(40 if failure == 'long_progress' else 8):
                save({'type': 'progress', 'step': step})
                time.sleep(.5)
        if failure == 'invalid_report':
            answer = 'malformed answer'
        if agent == 'codex':
            final = {'type': 'task_complete', 'last_agent_message': answer}
            if failure in ('quota', 'provider_error'):
                final = {'type': 'error', 'codex_error_info': 'usage_limit_exceeded' if failure == 'quota' else 'server_overloaded', 'message': failure}
            if failure == 'auth':
                final = {'type': 'error', 'codex_error_info': 'permission_denied', 'message': 'Usage limit reached; model_not_found'}
            if unavailable:
                final = {'type': 'error', 'codex_error_info': unavailable, 'message': unavailable}
            save({'type': 'event_msg', 'payload': final})
        else:
            final = {'type': 'assistant', 'message': {'stop_reason': 'end_turn', 'content': [{'type': 'text', 'text': answer}]}}
            if failure == 'auth':
                final.update(isApiErrorMessage=True, error='authentication_error')
                final['message']['content'][0]['text'] = 'Usage limit reached; model_not_found'
            if unavailable:
                final.update(isApiErrorMessage=True, error=unavailable)
                final['message']['content'][0]['text'] = unavailable
            save(final)
        print('READY ' + answer, flush=True)
        prompt = sys.stdin.readline()
        if not prompt:
            break
    elif agent == 'gemini':
        emit({'type': 'init', 'session_id': session})
        run_tool()
        if failure == 'auth':
            emit({'type': 'error', 'code': 'permission_denied', 'message': 'Usage limit reached; model_not_found'})
            emit({'type': 'result', 'status': 'error'})
            break
        if unavailable:
            emit({'type': 'error', 'code': unavailable, 'message': unavailable})
            emit({'type': 'result', 'status': 'error'})
            break
        emit({'type': 'message', 'role': 'assistant', 'content': answer})
        # An answer alone must not be accepted before the official final event.
        time.sleep(1)
        emit({'type': 'result', 'status': 'success'})
        break
    else:
        emit({'event': 'init', 'conversation_id': session})
        run_tool()
        if failure == 'auth':
            emit({'event': 'result', 'result': {'status': 'ERROR', 'code': 'invalid_api_key',
                  'conversation_id': session, 'response': 'Usage limit reached; model_not_found'}})
            break
        if unavailable:
            emit({'event': 'result', 'result': {'status': 'ERROR', 'code': unavailable,
                  'conversation_id': session, 'response': unavailable}})
            break
        emit({'event': 'step_update', 'step_update': {'state': 'DONE', 'text_delta': answer}})
        time.sleep(1)
        emit({'event': 'result', 'result': {'status': 'SUCCESS', 'conversation_id': session, 'response': answer}})
        break
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default=str(REPO / 'target/debug/flowmux'))
    parser.add_argument('--cli', default=str(REPO / 'target/debug/flowmuxctl'))
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
        path.write_text('#!' + sys.executable + '\n' + PROVIDER)
        path.chmod(0o700)
    h.env['PATH'] = str(native) + os.pathsep + str(Path(args.cli).parent) + os.pathsep + os.defpath
    task = h.root / 'task.txt'
    task.write_text('Read-only native fixture.')
    try:
        h.start_display()
        process, socket = h.window('recovery')
        workspace = h.rpc(socket, 'workspace_create', name='Team', root=str(h.root), team=True)['workspace_created']['id']
        source = h.workspace(socket, workspace)['panes'][0]['id']
        foreground = h.rpc(socket, 'workspace_create', name='Foreground', root=str(h.root))['workspace_created']['id']
        h.rpc(socket, 'workspace_focus', workspace=foreground)
        foreground_pane = h.workspace(socket, foreground)['panes'][0]['id']
        h.rpc(socket, 'pane_focus', pane=foreground_pane)
        with closing(display.Display(h.env['DISPLAY'])) as connection:
            atom = connection.intern_atom('_NET_WM_PID')
            window = next(w for w in connection.screen().root.query_tree().children
                          if (owner := w.get_full_property(atom, X.AnyPropertyType)) is not None
                          and int(owner.value[0]) == process.pid and w.get_geometry().width > 100)
            window.set_input_focus(X.RevertToParent, X.CurrentTime)
            connection.sync()
        time.sleep(.3)

        def type_native(text):
            with closing(display.Display(h.env['DISPLAY'])) as connection:
                for character in text:
                    code = connection.keysym_to_keycode(XK.string_to_keysym(character))
                    xtest.fake_input(connection, X.KeyPress, code)
                    xtest.fake_input(connection, X.KeyRelease, code)
                connection.sync()

        typed = 'recoverybefore'
        type_native(typed)
        fixture.wait_for(lambda: typed in h.screen(socket, foreground_pane), 'native foreground input before recovery')
        env = dict(h.env, FLOWMUX_SOCKET_PATH=str(socket), FLOWMUX_PANE_ID=source)

        def command(*argv):
            result = subprocess.run([sys.executable, str(HELPER), *argv], env=env,
                                    capture_output=True, text=True, timeout=25)
            assert result.returncode == 0, result.stdout + result.stderr
            return json.loads(result.stdout)

        def start(agent='codex', *extra, preserve_failures=False):
            # Keep unrelated fault cases independent of shared routing holds.
            if not preserve_failures:
                for directory in ('team-quota', 'team-unavailable'):
                    for cached in (h.root / 'state/flowmux' / directory).glob('*.json'):
                        cached.unlink()
            return command('start', '--fresh-session', '--agent', agent, '--role', 'recovery fixture', '--cwd', str(h.root),
                           '--task-file', str(task), '--cli', args.cli, '--stall-timeout', '3', '--timeout', '15', *extra)

        def finished(receipt):
            path = Path(receipt['job']) / 'status.json'
            return fixture.wait_for(lambda: (state if (state := json.loads(path.read_text()))['state']
                                            in ('completed', 'failed', 'blocked', 'timed_out') else None),
                                    'managed completion', timeout=45)

        def stopped(pid):
            stat = Path(f'/proc/{pid}/stat')
            return not stat.exists() or stat.read_text().split(') ', 1)[1].startswith('Z')

        wrapper = h.root / 'lost-ack-cli'
        wrapper.write_text('#!' + sys.executable + '\n' + f'''
import subprocess, sys
from pathlib import Path
root = Path(__file__).parent
result = subprocess.run([{args.cli!r}, *sys.argv[1:]], capture_output=True, text=True)
if 'team-spawn' in sys.argv:
    counter = root / 'spawn-count'
    count = int(counter.read_text()) + 1 if counter.exists() else 1
    counter.write_text(str(count))
    if count == 2 and result.returncode == 0:
        (root / 'lost-ack.json').write_text(result.stdout)
        sys.exit('Simulated IPC response loss after successful pane creation')
print(result.stdout, end='')
print(result.stderr, end='', file=sys.stderr)
sys.exit(result.returncode)
''')
        wrapper.chmod(0o700)
        (h.root / 'behavior.json').write_text(json.dumps({'codex': 'quota'}))
        receipt = start('codex', '--cli', str(wrapper))
        state = finished(receipt)
        assert state['blocker_kind'] == 'recovery_failed', state
        root = Path(receipt['job'])
        (h.root / 'lost-ack-before.json').write_text(json.dumps(state, indent=2))
        def replacement_exited():
            path = root / 'supervisor.json'
            supervisor = json.loads(path.read_text()) if path.exists() else {}
            return supervisor.get('generation') == 2 and stopped(supervisor['pid'])
        fixture.wait_for(replacement_exited, 'unconfirmed replacement worker exits', timeout=30)
        saved = json.loads((root / 'status.json').read_text())
        (h.root / 'lost-ack-after.json').write_text(json.dumps(saved, indent=2))
        assert {key: saved[key] for key in state} == state, saved
        assert saved['worker_error'] == 'Worker assignment was not confirmed', saved
        assert len(saved['attempts']) == 1 and saved['closed_panes'] == [receipt['pane']], saved
        for operation in (['status'], ['wait', '--timeout', '1'], ['_worker', '--generation', '2'],
                          ['_recover', '--generation', '1']):
            result = subprocess.run([sys.executable, str(HELPER), operation[0], str(root), *operation[1:]],
                                    env=env, capture_output=True, text=True, timeout=10)
            assert result.returncode == 1, result
            assert json.loads((root / 'status.json').read_text()) == saved
        assert (h.root / 'spawn-count').read_text() == '2'
        replacement = json.loads((h.root / 'lost-ack.json').read_text())['surface_created']['pane']
        panes = [p['id'] for p in h.workspace(socket, workspace)['panes']]
        assert receipt['pane'] not in panes and source in panes and replacement in panes
        assert not (root / 'attempt-2').exists(), 'Unconfirmed replacement must not run the task'
        assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == foreground
        h.pass_check('Lost recovery-spawn acknowledgement preserves diagnostics and history without replay')
        h.rpc(socket, 'pane_close', pane=replacement)

        for agent in ('codex', 'claude', 'gemini', 'agy'):
            (h.root / 'behavior.json').write_text(json.dumps({agent: 'auth'}))
            receipt = start(agent)
            state = finished(receipt)
            assert state['state'] == 'blocked' and state['blocker_kind'] == 'input_required', state
            assert len(state['attempts']) == 1 and not state['fallback_used'], state
            assert not state['closed_panes'] and state['pane'] == receipt['pane'], state
            assert receipt['pane'] in [p['id'] for p in h.workspace(socket, workspace)['panes']]
            assert not list((h.root / 'state/flowmux/team-quota').glob('*.json'))
            if agent in ('codex', 'claude'):
                assert not stopped(state['pid']), 'auth diagnostics must retain the live TUI'
            assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == foreground
            type_native('a')
            typed += 'a'
            fixture.wait_for(lambda: typed in h.screen(socket, foreground_pane), 'native input after auth failure')
            h.pass_check(f'{agent}: native auth code prevents quota holds, substitution and automatic pane closure')
            h.rpc(socket, 'pane_close', pane=receipt['pane'])

        for failure in ('quota', 'provider_error', 'stalled', 'crash', 'invalid_report'):
            (h.root / 'behavior.json').write_text(json.dumps({'codex': failure}))
            receipt = start()
            state = finished(receipt)
            assert state['state'] == 'completed' and state['agent'] == 'claude', state
            assert len(state['attempts']) == 2 and state['fallback_used'], state
            assert state['pane'] != receipt['pane'] and receipt['pane'] in state['closed_panes'], state
            panes = [p['id'] for p in h.workspace(socket, workspace)['panes']]
            assert receipt['pane'] not in panes and source in panes and state['pane'] in panes
            evidence = json.loads((h.root / (receipt['token'] + '-process.json')).read_text())
            fixture.wait_for(lambda: stopped(evidence['pid']) and stopped(evidence['child']), 'owned process tree stopped')
            assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == foreground
            type_native('x')
            typed += 'x'
            fixture.wait_for(lambda: typed in h.screen(socket, foreground_pane), 'native foreground input after recovery')
            h.pass_check(f'{failure}: old process tree and pane removed before replacement; foreground preserved')
            h.rpc(socket, 'pane_close', pane=state['pane'])

        for agent in ('codex', 'claude', 'gemini', 'agy'):
            (h.root / 'behavior.json').write_text(json.dumps({agent: 'quiet_tool'}))
            receipt = start(agent, '--stall-timeout', '2')
            state = finished(receipt)
            assert state['state'] == 'completed' and not state['fallback_used'], state
            assert state['pane'] == receipt['pane'] and not state['closed_panes'], state
            assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == foreground
            h.pass_check(f'{agent}: a native tool working silently past the stall deadline keeps its pane and completes')
            h.rpc(socket, 'pane_close', pane=state['pane'])

        (h.root / 'behavior.json').write_text(json.dumps({'codex': 'hung_tool'}))
        receipt = start('codex', '--stall-timeout', '1', '--timeout', '0')
        state = finished(receipt)
        assert state['state'] == 'completed' and state['agent'] == 'claude', state
        assert state['attempts'][0]['blocker_kind'] == 'stalled' and receipt['pane'] in state['closed_panes'], state
        evidence = json.loads((h.root / (receipt['token'] + '-process.json')).read_text())
        fixture.wait_for(lambda: stopped(evidence['pid']) and stopped(evidence['child']), 'hung native tool is stopped')
        h.pass_check('Hung native tools exhaust bounded grace, stop and close their pane before replacement')
        h.rpc(socket, 'pane_close', pane=state['pane'])

        (h.root / 'behavior.json').write_text(json.dumps({'agy': 'failed_tool'}))
        receipt = start('agy', '--stall-timeout', '2', '--timeout', '0')
        state = finished(receipt)
        first = state['attempts'][0]
        assert state['state'] == 'completed' and first['blocker_kind'] == 'stalled', state
        assert first['pending_tools'] == 0 and first['stall_timeout'] == 2, state
        assert receipt['pane'] in state['closed_panes'], state
        h.pass_check('Failed agy tools release grace so a subsequent stall uses the normal deadline')
        h.rpc(socket, 'pane_close', pane=state['pane'])

        for agent in ('codex', 'claude', 'gemini', 'agy'):
            for kind in ('model_unavailable', 'client_unavailable'):
                (h.root / 'behavior.json').write_text(json.dumps({agent: kind}))
                receipt = start(agent)
                state = finished(receipt)
                assert state['state'] == 'completed' and state['attempts'][0]['blocker_kind'] == kind, state
                assert receipt['pane'] in state['closed_panes'], state
                h.rpc(socket, 'pane_close', pane=state['pane'])
                (h.root / 'behavior.json').write_text('{}')
                next_job = start(agent, preserve_failures=True)
                assert next_job['agent'] != agent, next_job
                state = finished(next_job)
                assert state['state'] == 'completed' and len(state['attempts']) == 1, state
                assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == foreground
                h.pass_check(f'{agent}: {kind} is shared with the next job before any failed worker launches')
                h.rpc(socket, 'pane_close', pane=state['pane'])

        (native / 'codex').write_text('#!/missing/flowmux-test-interpreter\n')
        receipt = start()
        state = finished(receipt)
        assert state['state'] == 'completed' and state['attempts'][0]['blocker_kind'] == 'launch_error', state
        assert receipt['pane'] in state['closed_panes']
        h.rpc(socket, 'pane_close', pane=state['pane'])
        (native / 'codex').write_text('#!' + sys.executable + '\n' + PROVIDER)
        h.pass_check('An unlaunchable provider closes its failed pane and selects another installed provider')

        (h.root / 'behavior.json').write_text(json.dumps({'codex': 'invalid_report', 'claude': 'invalid_report'}))
        receipt = start('codex', '--max-attempts', '2')
        state = finished(receipt)
        assert state['state'] == 'failed' and state['recovery_exhausted'], state
        assert len(state['attempts']) == 2 and len(state['closed_panes']) == 2, state
        h.pass_check('Attempt budget stops repeated malformed reports and closes both failed panes')

        (h.root / 'behavior.json').write_text(json.dumps({'codex': 'quota'}))
        receipt = start('codex', '--no-fallback')
        state = finished(receipt)
        assert state['state'] == 'blocked' and state['recovery_exhausted'], state
        assert receipt['pane'] not in [p['id'] for p in h.workspace(socket, workspace)['panes']]
        h.pass_check('No-fallback closes the failed managed pane and stops without launching another model')

        (h.root / 'behavior.json').write_text('{}')
        for agent in ('gemini', 'agy'):
            receipt = start(agent)
            state = finished(receipt)
            assert state['state'] == 'completed' and state['session_id'], state
            turn = command('followup', receipt['job'], '--task-file', str(task), '--cli', args.cli)
            following = finished(turn)
            assert following['state'] == 'completed' and following['session_id'] == state['session_id'], following
            assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == foreground
            h.pass_check(f'{agent}: official result required; YOLO follow-up resumes the exact conversation')
            h.rpc(socket, 'pane_close', pane=state['pane'])

        receipt = start('codex')
        state = finished(receipt)
        h.rpc(socket, 'agent_activity_update', pane=state['pane'], surface=state['surface'], agent='codex',
              activity='idle', session_id=state['session_id'], source='flowmux:hook')
        (h.root / 'behavior.json').write_text(json.dumps({'codex': 'quota'}))
        turn = command('followup', receipt['job'], '--task-file', str(task), '--cli', args.cli)
        following = finished(turn)
        assert following['state'] == 'completed' and following['agent'] == 'claude', following
        assert following['pane'] != state['pane'] and state['pane'] in following['closed_panes']
        assert json.loads((Path(receipt['job']) / 'status.json').read_text())['session_id'] == state['session_id']
        assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == foreground
        h.pass_check('Follow-up quota recovery replaces its pane while preserving earlier reports and foreground')
        h.rpc(socket, 'pane_close', pane=following['pane'])

        (h.root / 'behavior.json').write_text('{}')
        receipt = start('codex', '--timeout', '2')
        state = finished(receipt)
        h.rpc(socket, 'agent_activity_update', pane=state['pane'], surface=state['surface'], agent='codex',
              activity='idle', session_id=state['session_id'], source='flowmux:hook')
        (h.root / 'behavior.json').write_text(json.dumps({'codex': 'progress'}))
        turn = command('followup', receipt['job'], '--task-file', str(task), '--cli', args.cli,
                       '--timeout', '0', '--stall-timeout', '2')
        following = finished(turn)
        assert following['state'] == 'completed' and following['pane'] == state['pane'], following
        assert len(following['attempts']) == 1 and not following['fallback_used'], following
        h.pass_check('A progressing follow-up outlives the original budget when its total limit is disabled')
        h.rpc(socket, 'agent_activity_update', pane=state['pane'], surface=state['surface'], agent='codex',
              activity='idle', session_id=state['session_id'], source='flowmux:hook')
        turn = command('followup', receipt['job'], '--task-file', str(task), '--cli', args.cli, '--timeout', '1')
        limited = finished(turn)
        assert limited['state'] == 'timed_out' and limited['blocker_kind'] == 'time_budget', limited
        assert len(limited['attempts']) == 1 and limited['recovery_exhausted'], limited
        assert state['pane'] not in [p['id'] for p in h.workspace(socket, workspace)['panes']]
        h.pass_check('Explicit time budget closes the owned worker without labeling progress stalled or starting a replacement')

        (h.root / 'behavior.json').write_text(json.dumps({'codex': 'stalled'}))
        receipt = start('codex', '--timeout', '1', '--stall-timeout', '1')
        limited = finished(receipt)
        assert limited['blocker_kind'] == 'time_budget' and len(limited['attempts']) == 1, limited
        assert receipt['pane'] in limited['closed_panes'] and limited['recovery_exhausted'], limited
        h.pass_check('Equal time and stall limits stop without substitution')

        (h.root / 'behavior.json').write_text(json.dumps({'codex': 'long_progress'}))
        receipt = start('codex', '--timeout', '6')
        root = Path(receipt['job'])
        evidence_path = h.root / (receipt['token'] + '-process.json')
        evidence = fixture.wait_for(lambda: json.loads(evidence_path.read_text()) if evidence_path.exists() else None,
                                    'provider begins timed work')
        fixture.wait_for(lambda: time.monotonic() - evidence['started'] >= 4, 'four seconds of active work')
        identity = dict(pane=receipt['pane'], surface=receipt['surface'], agent='codex',
                        session_id=evidence['session'], source='flowmux:hook')
        h.rpc(socket, 'agent_activity_update', **identity, activity='needs_input')
        fixture.wait_for(lambda: json.loads((root / 'status.json').read_text())['state'] == 'waiting_input', 'input wait pauses budget')
        time.sleep(3)
        assert json.loads((root / 'status.json').read_text())['state'] == 'waiting_input'
        resumed = time.monotonic()
        h.rpc(socket, 'agent_activity_update', **identity, activity='running')
        limited = fixture.wait_for(lambda: (s if (s := json.loads((root / 'status.json').read_text()))['state'] == 'timed_out' else None),
                                   'remaining budget expires without resetting six seconds', timeout=4)
        assert time.monotonic() - resumed < 4 and limited['blocker_kind'] == 'time_budget', limited
        assert len(limited['attempts']) == 1 and receipt['pane'] in limited['closed_panes'], limited
        h.pass_check('Input waits preserve consumed budget and close the worker after only its remaining time')

        (h.root / 'behavior.json').write_text('{}')
        for agent in ('codex', 'claude'):
            receipt = start(agent)
            state = finished(receipt)
            h.rpc(socket, 'agent_activity_update', pane=state['pane'], surface=state['surface'], agent=agent,
                  activity='idle', session_id=state['session_id'], source='flowmux:hook')
            root = Path(receipt['job'])
            supervisor = json.loads((root / 'supervisor.json').read_text())
            os.kill(supervisor['pid'], signal.SIGKILL)
            fixture.wait_for(lambda: stopped(supervisor['pid']), 'test-owned supervisor exited')
            assert not stopped(state['pid']), 'provider must remain live to reproduce orphaned collection'
            rejected = subprocess.run([sys.executable, str(HELPER), 'followup', str(root),
                                      '--task-file', str(task), '--cli', args.cli], env=env,
                                     capture_output=True, text=True, timeout=25)
            assert rejected.returncode == 1 and 'supervisor is no longer live' in rejected.stderr, rejected
            assert not (root / 'turns').exists()
            assert 'FLOWMUX_TEAM_JOB:' not in h.screen(socket, state['pane'])
            assert json.loads((root / 'status.json').read_text())['state'] == 'completed'
            h.rpc(socket, 'pane_close', pane=state['pane'])
            h.pass_check(f'{agent}: live idle agent receives no follow-up after its supervisor exits')
        assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == foreground
        type_native('y')
        typed += 'y'
        fixture.wait_for(lambda: typed in h.screen(socket, foreground_pane), 'native foreground input after supervision checks')
    finally:
        h.cleanup()


if __name__ == '__main__':
    main()
