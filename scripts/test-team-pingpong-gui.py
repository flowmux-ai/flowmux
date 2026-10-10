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
import subprocess
import sys

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
    parser.add_argument('--cases', nargs='+', choices=('review', 'clarify', 'repair'), default=['review', 'clarify', 'repair'])
    parser.add_argument('--timeout', type=int, default=180)
    args = parser.parse_args()
    args.protected_pid = []
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
    try:
        h.start_display()
        process, socket = h.window('pingpong')
        for agent in ([args.real_agent] if args.real_agent else ['claude', 'codex']):
            for case in args.cases:
                workspace = h.rpc(socket, 'workspace_create', name=f'Team {agent}: {case}',
                                  root=str(REPO), team=True)['workspace_created']['id']
                pane = h.workspace(socket, workspace)['panes'][0]['id']
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
                if result.returncode:
                    raise RuntimeError(f'{agent}/{case} failed; see {log} and *.screen.txt')
                h.pass_check(f'{agent}/{case}: same-session ping-pong and semantic checks in real GUI PTYs')
        h.close_window(process)
    finally:
        h.cleanup()


if __name__ == '__main__':
    main()
