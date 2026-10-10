#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run the installed-style Team examples in isolated real Flowmux PTYs.

Default: deterministic provider fixtures. --real-agent uses the named installed
CLI and account, without changing credentials, hook trust or provider settings.
All windows, XDG state and editable example files belong to this test.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import uuid

from PIL import Image
from Xlib import X, display

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('fixture', REPO / 'scripts/test-ssh-workspace-gui.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)

PROVIDER = r'''
import json, os, pathlib, re, socket, sys, time
agent = pathlib.Path(sys.argv[0]).name
prompt = sys.argv[-1]
session = re.search(r'FLOWMUX_TEAM_JOB:(\S+)', prompt)[1]
profile = pathlib.Path(os.environ['CODEX_HOME' if agent == 'codex' else 'CLAUDE_CONFIG_DIR'])
transcript = profile / ('sessions/2026/10/10' if agent == 'codex' else 'projects/fixture') / (session + '.jsonl')
transcript.parent.mkdir(parents=True, exist_ok=True)
def event(value):
    with transcript.open('a') as out:
        out.write(json.dumps(value) + '\n')
def activity(value):
    request = dict(id=1, kind='request', verb='agent_activity_update',
        pane=os.environ['FLOWMUX_PANE_ID'], surface=os.environ['FLOWMUX_SURFACE_ID'],
        agent=agent, activity=value, session_id=session, pid=os.getpid(), source='flowmux:hook')
    with socket.socket(socket.AF_UNIX) as stream:
        stream.connect(os.environ['FLOWMUX_SOCKET_PATH'])
        stream.sendall((json.dumps(request) + '\n').encode())
        stream.makefile().readline()
if agent == 'codex':
    event(dict(type='session_meta', payload=dict(id=session)))
while prompt:
    path = pathlib.Path(re.search(r'(/\S+/(?:task|prompt)\.txt)', prompt)[1])
    task = path.with_name('task.txt').read_text()
    if agent == 'codex':
        event(dict(type='event_msg', payload=dict(type='user_message', message=prompt)))
    else:
        event(dict(type='user', sessionId=session, message=dict(content=prompt)))
    activity('running')
    print('WORKING ' + agent + ': ' + task[:90], flush=True)
    time.sleep(.3)
    if 'approval fixture' in task:
        print('Approval required: inspect fixture command before continuing', flush=True)
        activity('needs_input')
        sys.stdin.readline()
        raise SystemExit('No approval automation is expected in this test')
    outcome = 'completed'
    if 'capacity.txt' in task:
        source = pathlib.Path(re.search(r'(/\S+/capacity\.txt)', task)[1])
        if 'unspecified' in source.read_text():
            outcome, report = 'blocked', 'Requested seats is unspecified; supply that input.'
        else:
            report = '25 - 9 = 16 remaining seats. Source: ' + str(source)
    elif 'shipping.py' in task:
        source = pathlib.Path(re.search(r'(/\S+/shipping\.py)', task)[1])
        if 'Fix only' in task:
            source.write_text(source.read_text().replace('> 50', '>= 50'))
            report = 'Corrected inclusive boundary in ' + str(source)
        else:
            report = 'Current code requires total > 50: ' + str(source)
    else:
        source = pathlib.Path(re.search(r'(/\S+/service\.txt)', task)[1])
        report = 'Verified Flowmux capacity 25' + (', region Seoul' if 'region' in task else '') + '. Source: ' + str(source)
    answer = json.dumps(dict(task_status=outcome, report=report))
    if agent == 'codex':
        event(dict(type='event_msg', payload=dict(type='task_complete', last_agent_message=answer)))
    else:
        event(dict(type='assistant', message=dict(stop_reason='end_turn', content=[dict(type='text', text=answer)])))
    print('RESULT ' + answer, flush=True)
    activity('idle')
    print('READY for followup', flush=True)
    prompt = sys.stdin.readline().strip()
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default=str(REPO / 'target/debug/flowmux'))
    parser.add_argument('--cli', default=str(REPO / 'target/debug/flowmuxctl'))
    parser.add_argument('--real-agent', choices=('claude', 'codex'))
    parser.add_argument('--expect-input', action='store_true', help='Verify a real provider input/approval handoff, without approving')
    parser.add_argument('--cases', nargs='+', choices=('review', 'clarify', 'repair'), default=['review', 'clarify', 'repair'])
    parser.add_argument('--timeout', type=int, default=180)
    parser.add_argument('--protected-pid', type=int, action='append', default=[])
    args = parser.parse_args()
    if args.expect_input and not args.real_agent:
        parser.error('--expect-input requires --real-agent')
    h = fixture.Harness(args)
    for key in ('CODEX_THREAD_ID', 'CODEX_SESSION_ID', 'CLAUDECODE'):
        h.env.pop(key, None)
    helper = REPO / '.agents/skills/flowmux-team/scripts/team.py'
    examples = helper.with_name('examples.py')
    options = h.root / 'config/flowmux/options.json'
    options.parent.mkdir(parents=True)
    options.write_text(json.dumps(dict(default_shell='/bin/sh', auto_resume_agent_sessions=False)))
    if not args.real_agent:
        bin_dir = h.root / 'bin'
        bin_dir.mkdir()
        for agent in ('claude', 'codex'):
            path = bin_dir / agent
            path.write_text('#!/usr/bin/env python3\n' + PROVIDER)
            path.chmod(0o700)
        h.env.update(PATH=str(bin_dir) + os.pathsep + h.env['PATH'],
                     CLAUDE_CONFIG_DIR=str(h.root / 'claude'), CODEX_HOME=str(h.root / 'codex'))
    elif args.real_agent == 'codex':
        # Test the built legacy hook without changing the user's installed config or trust.
        executable = shutil.which('codex')
        if not executable:
            raise RuntimeError('Codex is not installed')
        bin_dir = h.root / 'bin'
        bin_dir.mkdir()
        wrapper = bin_dir / 'codex'
        notify = 'notify=' + json.dumps([args.cli, 'hooks', 'codex', 'stop'])
        wrapper.write_text('#!/bin/sh\nexport PATH=' + shlex.quote(h.env['PATH'])
                           + '\nexec ' + shlex.join([executable, '-c', notify]) + ' "$@"\n')
        wrapper.chmod(0o700)
        h.env['PATH'] = str(bin_dir) + os.pathsep + h.env['PATH']
    try:
        h.start_display()
        process, socket = h.window('pingpong')
        for agent in ([args.real_agent] if args.real_agent else ['claude', 'codex']):
            for case in args.cases:
                workspace = h.rpc(socket, 'workspace_create', name=f'Team {agent}: {case}',
                                  root=str(REPO), team=True)['workspace_created']['id']
                pane = h.workspace(socket, workspace)['panes'][0]['id']
                other_workspace = h.rpc(socket, 'workspace_create', name=f'Other Team {case}',
                                        root=str(REPO), team=True)['workspace_created']['id']
                other_before = h.workspace(socket, other_workspace)
                env = dict(h.env, FLOWMUX_SOCKET_PATH=str(socket), FLOWMUX_PANE_ID=pane,
                           FLOWMUX_BUNDLED_CLI_PATH=args.cli)
                log = h.root / f'{agent}-{case}.log'
                command = [sys.executable, str(examples), '--case', case, '--agent', agent,
                           '--cwd', str(REPO), '--cli', args.cli, '--timeout', str(args.timeout)]
                with log.open('w') as out:
                    result = subprocess.run(command, env=env, stdout=out, stderr=subprocess.STDOUT)
                print(log.read_text(), flush=True)
                workspace_tree = h.workspace(socket, workspace)
                for worker in workspace_tree['panes']:
                    (h.root / f"{agent}-{case}-{worker['id']}.screen.txt").write_text(h.screen(socket, worker['id']))
                (h.root / f'{agent}-{case}-tree.json').write_text(json.dumps(workspace_tree, indent=2))
                connection = display.Display(h.env['DISPLAY'])
                try:
                    window = connection.screen().root
                    size = window.get_geometry()
                    raw = window.get_image(0, 0, size.width, size.height, X.ZPixmap, 0xFFFFFFFF)
                    Image.frombytes('RGB', (size.width, size.height), raw.data, 'raw', 'BGRX').save(h.root / f'{agent}-{case}.png')
                finally:
                    connection.close()
                if args.expect_input:
                    artifacts = Path(next(line.removeprefix('ARTIFACTS: ') for line in log.read_text().splitlines()
                                          if line.startswith('ARTIFACTS: ')))
                    run = json.loads((artifacts / 'run.json').read_text())
                    handoff = run['turns'][-1]['status']
                    assert handoff['state'] == 'waiting_input' and 'task_status' not in handoff, handoff
                    worker = handoff['input_required']
                    assert worker['source_pane'] == pane and worker['workspace'] == workspace, worker
                    assert any(p['id'] == worker['pane'] and any(t.get('agent', {}).get('status') == 'blocked'
                               for t in p['tabs']) for p in workspace_tree['panes']), workspace_tree
                    assert not (Path(handoff['job']) / 'result.md').exists()
                    h.pass_check(f'{agent}: real provider input UI is blocked, handed to its lead, and not approved')
                    continue
                if result.returncode:
                    raise RuntimeError(f'{agent}/{case} failed; see {log} and *.screen.txt')
                assert h.workspace(socket, other_workspace)['panes'] == other_before['panes']
                if not args.real_agent:
                    artifacts = Path(next(line.removeprefix('ARTIFACTS: ') for line in log.read_text().splitlines()
                                          if line.startswith('ARTIFACTS: ')))
                    run = json.loads((artifacts / 'run.json').read_text())
                    worker = next(iter(run['workers'].values()))
                    before = (Path(worker['job']) / 'result.md').read_bytes()
                    wrong_env = dict(env, FLOWMUX_PANE_ID=other_before['panes'][0]['id'])
                    rejected = subprocess.run([sys.executable, str(helper), 'followup', worker['job'],
                        '--task-file', str(artifacts / 'request-1.txt'), '--cli', args.cli],
                        env=wrong_env, capture_output=True, text=True, timeout=20)
                    assert rejected.returncode and 'original lead pane' in rejected.stderr, rejected.stderr
                    assert (Path(worker['job']) / 'result.md').read_bytes() == before
                h.pass_check(f'{agent}/{case}: same-session ping-pong and semantic checks in real GUI PTYs')
            if not args.real_agent:
                # Resolve a lead session even when inherited context and focus point at another Team.
                source = h.workspace(socket, workspace)['panes'][0]
                session = str(uuid.uuid4())
                h.rpc(socket, 'agent_activity_update', pane=pane, surface=source['tabs'][0]['id'],
                      agent='codex', activity='running', session_id=session, source='flowmux:hook')
                h.rpc(socket, 'workspace_focus', workspace=other_workspace)
                env.update(CODEX_THREAD_ID=session, FLOWMUX_PANE_ID=other_before['panes'][0]['id'])
                task = h.root / f'{agent}-approval.txt'
                task.write_text('Show approval fixture and wait for a human. Do not complete.')
                started = subprocess.run([sys.executable, str(helper), 'start', '--agent', agent,
                    '--complexity', 'simple', '--role', 'approval fixture', '--cwd', str(REPO),
                    '--task-file', str(task), '--cli', args.cli, '--timeout', '300', '--no-fallback'],
                    env=env, capture_output=True, text=True, check=True, timeout=30)
                worker = json.loads(started.stdout)
                assert worker['source_pane'] == pane and worker['workspace'] == workspace, worker
                waiting = subprocess.run([sys.executable, str(helper), 'wait', worker['job'], '--timeout', '30'],
                                         env=env, capture_output=True, text=True, timeout=40)
                (h.root / f'{agent}-approval-handoff.json').write_text(waiting.stdout)
                assert waiting.returncode == 2, waiting.stdout + waiting.stderr
                handoff = json.loads(waiting.stdout)
                assert handoff['state'] == 'waiting_input' and 'task_status' not in handoff, handoff
                assert handoff['input_required']['pane'] == worker['pane'], handoff
                assert handoff['input_required']['source_pane'] == pane, handoff
                assert 'Approval required' in h.screen(socket, worker['pane'])
                assert len(h.workspace(socket, other_workspace)['panes']) == len(other_before['panes'])
                h.pass_check(f'{agent}: stale pane and other Team focus retain lead; approval handoff targets exact worker')
        h.close_window(process)
    finally:
        h.cleanup()


if __name__ == '__main__':
    main()
