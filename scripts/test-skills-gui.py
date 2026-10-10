#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise Skills through AT-SPI in an isolated live Flowmux window (Linux).

Requires Xvfb, D-Bus, at-spi2-core, python3-xlib, python3-pil, python3-gi and gir1.2-atspi-2.0.
Only test-owned provider config roots are mutated; HOME and real agent files stay
unchanged. Screenshots, backups and task artifacts remain in the printed folder.
"""
import argparse
from contextlib import closing
import importlib.util
import json
import os
import shlex
import shutil
import uuid
from pathlib import Path
import subprocess
import sys
import time

sys.dont_write_bytecode = True
from PIL import Image
from Xlib import X, display
from Xlib.ext import xtest

REPO = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('fixture', REPO / 'scripts/test-ssh-workspace-gui.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def team_lifecycle(h):
    """Use native CLI-shaped fixtures to verify profile, restore and shutdown in real PTYs."""
    bin_dir = h.root / 'bin'
    native_dir = h.root / 'lead-bin'
    native_dir.mkdir()
    for agent in ('claude', 'codex'):
        wrapper = bin_dir / agent
        wrapper.write_text('#!/bin/sh\nexec fixture-' + agent + ' "$@"\n')
        wrapper.chmod(0o700)
        executable = native_dir / ('fixture-' + agent)
        executable.write_text('#!/usr/bin/env python3\n' + r'''
import json, os, pathlib, re, sys, time
agent = pathlib.Path(sys.argv[0]).name.removeprefix('fixture-')
root = pathlib.Path(os.environ['XDG_STATE_HOME']).parent
profile = pathlib.Path(os.environ['CODEX_HOME' if agent == 'codex' else 'CLAUDE_CONFIG_DIR'])
if '--resume' in sys.argv or 'resume' in sys.argv:
    (root / (agent + '-resumed.json')).write_text(json.dumps({'argv': sys.argv, 'profile': str(profile)}))
    print('RESUMED ' + agent, flush=True)
    time.sleep(120)
    sys.exit()
if 'app-server' in sys.argv:
    sys.exit()
token = re.search(r'FLOWMUX_TEAM_JOB:([^\s]+)', sys.argv[-1])[1]
if (root / 'interrupt-worker').exists():
    print('WAITING for interruption', flush=True)
    time.sleep(120)
    sys.exit()
answer = json.dumps({'task_status': 'completed', 'report': 'profile verified'})
if agent == 'codex':
    path = profile / 'sessions/2026/10/09' / (token + '.jsonl')
    user = {'type': 'response_item', 'payload': {'role': 'user', 'content': sys.argv[-1]}}
    final = {'type': 'event_msg', 'payload': {'type': 'task_complete', 'last_agent_message': answer}}
    if (root / 'provider-error.json').exists():
        native_error = json.loads((root / 'provider-error.json').read_text())
        error = native_error['error']
        final['payload'] = ({'type': 'error', **error} if native_error['standalone'] else
                            {'type': 'task_complete', 'last_agent_message': None, 'error': error})
        print('PROVIDER ERROR ' + error['codex_error_info'], flush=True)
else:
    path = profile / 'projects/fixture' / (token + '.jsonl')
    user = {'type': 'user', 'sessionId': token, 'message': {'content': sys.argv[-1]}}
    final = {'type': 'assistant', 'message': {'stop_reason': 'end_turn', 'content': [{'type': 'text', 'text': answer}]}}
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(json.dumps(user) + '\n' + json.dumps(final) + '\n')
print('PROFILE VERIFIED ' + agent, flush=True)
time.sleep(120)
''')
        executable.chmod(0o700)
    h.env['PATH'] = os.pathsep.join([str(bin_dir), str(Path(h.args.cli).parent), os.defpath])
    options = h.root / 'config/flowmux/options.json'
    config = json.loads(options.read_text()) if options.exists() else {}
    config.update(auto_resume_agent_sessions=True, default_shell='/bin/sh')
    options.parent.mkdir(parents=True, exist_ok=True)
    options.write_text(json.dumps(config))
    process, socket = h.window('lifecycle')
    workspace = h.rpc(socket, 'workspace_create', name='Team lifecycle', root=str(h.root), team=True)['workspace_created']['id']
    pane = h.workspace(socket, workspace)['panes'][0]['id']
    helper = h.root / 'claude/skills/flowmux-team/scripts/team.py'
    task = h.root / 'lifecycle-task.txt'
    task.write_text('Read-only fixture')
    env = dict(h.env, FLOWMUX_SOCKET_PATH=str(socket), FLOWMUX_PANE_ID=pane,
               PATH=str(native_dir) + os.pathsep + h.env['PATH'],
               CODEX_HOME=str(h.root / 'lead-codex'), CLAUDE_CONFIG_DIR=str(h.root / 'lead-claude'))
    transient = h.root / 'worker-tmp'
    transient.mkdir()
    env['TMPDIR'] = str(transient)

    def start(agent):
        result = subprocess.run([sys.executable, str(helper), 'start', '--agent', agent,
                                 '--role', 'Lifecycle ' + agent, '--cwd', str(h.root),
                                 '--task-file', str(task), '--cli', h.args.cli],
                                env=env, capture_output=True, text=True, timeout=20)
        assert result.returncode == 0, result.stderr
        return json.loads(result.stdout)

    jobs = [start(agent) for agent in ('claude', 'codex')]
    for job in jobs:
        status_path = Path(job['job']) / 'status.json'
        fixture.wait_for(lambda: json.loads(status_path.read_text())['state'] == 'completed', 'profile report')
        status = json.loads(status_path.read_text())
        assert str(h.root / ('lead-' + job['agent'])) in status['transcript'], status
        fixture.wait_for(lambda: 'PROFILE VERIFIED' in h.screen(socket, job['pane']), 'visible profile completion')
    h.pass_check('Both providers launch through PATH-dispatching wrappers with lead-only binaries and profiles')

    for standalone, code in [(True, 'usage_limit_exceeded'), (False, 'usage_limit_exceeded'),
                             (False, 'server_overloaded')]:
        quota = code == 'usage_limit_exceeded'
        error = {'codex_error_info': code, 'message': 'You’ve hit your usage limit.' if quota else 'Selected model is at capacity.'}
        (h.root / 'provider-error.json').write_text(json.dumps({'standalone': standalone, 'error': error}))
        before_error = h.workspace(socket, workspace)
        job = start('codex')
        status_path = Path(job['job']) / 'status.json'
        fixture.wait_for(lambda: json.loads(status_path.read_text())['state'] in ('completed', 'blocked'), 'native error result')
        status = json.loads(status_path.read_text())
        assert status['state'] == ('completed' if quota else 'blocked'), status
        assert status['fallback_used'] == quota, status
        assert len(status['attempts']) == (2 if quota else 1), status
        if quota:
            assert status['agent'] == 'claude', status
            fixture.wait_for(lambda: 'switching to claude' in h.screen(socket, job['pane']), 'visible provider substitution')
        else:
            assert status['blocker_kind'] == 'provider_error', status
            assert (Path(job['job']) / 'result.md').read_text() == error['message']
        after_error = h.workspace(socket, workspace)
        assert len(after_error['panes']) == len(before_error['panes']) + 1
        worker_pane = next(p for p in after_error['panes'] if p['id'] == job['pane'])
        assert len(worker_pane['tabs']) == 1, worker_pane
        (h.root / 'provider-error.json').unlink()
    h.pass_check('Native Codex quota errors switch to Claude in the same pane/tab; nested capacity errors stop with their cause')

    (h.root / 'interrupt-worker').touch()
    interrupted = start('claude')
    fixture.wait_for(lambda: 'WAITING for interruption' in h.screen(socket, interrupted['pane']), 'live interrupted worker')
    before = h.workspace(socket, workspace)
    h.close_window(process)
    result = subprocess.run([sys.executable, str(helper), 'wait', interrupted['job'], '--timeout', '5'],
                            capture_output=True, text=True, timeout=10)
    assert result.returncode == 1, result.stdout + result.stderr
    assert json.loads(result.stdout)['blocker_kind'] == 'interrupted', result.stdout
    h.pass_check('Closing the isolated GUI turns an unfinished worker into a terminal interrupted failure')
    (h.root / 'interrupt-worker').unlink()

    sessions = h.root / 'data/flowmux/agent-sessions'
    sessions.mkdir(parents=True, exist_ok=True)
    # Native fixtures have no installed provider hooks; seed their saved session bindings.
    for job in jobs:
        (sessions / (job['agent'] + '.json')).write_text(json.dumps({job['surface']: job['token']}))
    shutil.rmtree(transient)
    helper.rename(helper.with_suffix('.disabled'))
    process, socket = h.window('lifecycle-restored')
    for job in jobs:
        evidence = h.root / (job['agent'] + '-resumed.json')
        fixture.wait_for(evidence.exists, 'native resume ' + job['agent'])
        resume = json.loads(evidence.read_text())
        assert job['token'] in resume['argv'], resume
        assert resume['profile'] == str(h.root / ('lead-' + job['agent'])), resume
        argv = resume['argv']
        if job['agent'] == 'codex':
            assert '--no-daemon' in argv and argv[argv.index('--sandbox') + 1] == 'read-only', argv
        else:
            assert '--session-id' not in argv and argv[argv.index('--tools') + 1] == 'Read,Glob,Grep', argv
            assert argv[argv.index('--permission-mode') + 1] == 'dontAsk', argv
        fixture.wait_for(lambda: 'RESUMED ' + job['agent'] in h.screen(socket, job['pane']), 'visible resumed agent')
        assert len(list(Path(job['job']).glob('attempt-*'))) == 1
        assert json.loads((Path(job['job']) / 'status.json').read_text())['state'] == 'completed'
    after = h.workspace(socket, workspace)
    assert [p['id'] for p in after['panes']] == [p['id'] for p in before['panes']]
    fixture.wait_for(lambda: 'No task was repeated' in h.screen(socket, interrupted['pane']), 'interrupted assignment not repeated')
    h.pass_check('Claude and Codex resume in the same panes with lead profiles; original tasks and final results stay unchanged')
    h.pass_check('Worker restoration survives temporary cleanup and removal of the installed helper')
    helper.with_suffix('.disabled').rename(helper)
    h.close_window(process)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default=str(REPO / 'target/debug/flowmux'))
    parser.add_argument('--cli', default=str(REPO / 'target/debug/flowmuxctl'))
    args = parser.parse_args()
    args.protected_pid = []
    h = fixture.Harness(args)
    h.env.update(GTK_A11Y='atspi', CLAUDE_CONFIG_DIR=str(h.root / 'claude'), CODEX_HOME=str(h.root / 'codex'))
    h.env.pop('AT_SPI_BUS_ADDRESS', None)
    h.env.pop('CODEX_THREAD_ID', None)
    h.env.pop('CODEX_SESSION_ID', None)
    team = h.root / 'claude/skills/flowmux-team'
    browser = h.root / 'claude/skills/flowmux-browser/SKILL.md'
    source = REPO / '.agents/skills/flowmux-team'
    files = ['SKILL.md', 'scripts/team.py', 'references/sample.md']
    (h.root / 'claude').mkdir()
    try:
        h.start_display()
        os.environ['DISPLAY'] = h.env['DISPLAY']
        os.environ['DBUS_SESSION_BUS_ADDRESS'] = h.env['DBUS_SESSION_BUS_ADDRESS']
        os.environ.pop('AT_SPI_BUS_ADDRESS', None)
        import gi
        gi.require_version('Atspi', '2.0')
        from gi.repository import Atspi
        process, socket = h.window('skills')

        def walk(node):
            yield node
            for i in range(node.get_child_count()):
                child = node.get_child_at_index(i)
                if child:
                    yield from walk(child)

        def find(name, role=None, parent=None):
            return next((node for node in walk(parent or Atspi.get_desktop(0))
                         if node.get_name() == name and (role is None or node.get_role() == role)), None)

        def button(name, parent=None):
            return fixture.wait_for(lambda: find(name, Atspi.Role.PUSH_BUTTON, parent), name)

        def click(node):
            assert node.get_state_set().contains(Atspi.StateType.SENSITIVE), node.get_name()
            assert node.get_action_iface().do_action(0), node.get_name()

        def screenshot(name):
            time.sleep(.2)
            with closing(display.Display(h.env['DISPLAY'])) as connection:
                root = connection.screen().root
                size = root.get_geometry()
                raw = root.get_image(0, 0, size.width, size.height, X.ZPixmap, 0xFFFFFFFF)
                Image.frombytes('RGB', (size.width, size.height), raw.data, 'raw', 'BGRX').save(h.root / name)

        def scroll_to_team():
            bounds = find('Options', Atspi.Role.FRAME).get_component_iface().get_extents(Atspi.CoordType.SCREEN)
            with closing(display.Display(h.env['DISPLAY'])) as connection:
                xtest.fake_input(connection, X.MotionNotify, x=bounds.x + bounds.width - 35, y=bounds.y + bounds.height // 2)
                for _ in range(8):
                    xtest.fake_input(connection, X.ButtonPress, 5)
                    xtest.fake_input(connection, X.ButtonRelease, 5)
                connection.sync()

        click(button('Options'))
        click(fixture.wait_for(lambda: find('Skills', Atspi.Role.PAGE_TAB), 'Skills tab'))
        group = fixture.wait_for(lambda: find('Flowmux Team', Atspi.Role.LIST), 'Team group')
        assert [group.get_child_at_index(i).get_name() for i in range(group.get_child_count())] == ['Claude Code', 'Codex']
        row = find('Claude Code', Atspi.Role.LIST_ITEM, group)
        cli_group = find('Flowmux CLI', Atspi.Role.LIST)
        cli_row = find('Claude Code', Atspi.Role.LIST_ITEM, cli_group)
        assert not team.exists(), 'Viewing Skills must not install'
        screenshot('skills-initial.png')
        click(button('Install', cli_row))
        fixture.wait_for(browser.exists, 'CLI installed')
        click(button('Install', row))
        button('Installed', row)
        for name in files:
            assert (team / name).read_bytes() == (source / name).read_bytes(), name
        scroll_to_team()
        screenshot('skills-installed.png')
        h.pass_check('Both skill groups install independently; complete team bundle matches source')

        (team / 'scripts/team.py').write_text('local custom helper')
        (team / 'references/sample.md').unlink()
        click(button('Refresh status'))
        click(button('Update', row))
        button('Installed', row)
        for name in files:
            assert (team / name).read_bytes() == (source / name).read_bytes(), name
        backups = list((team / 'scripts').glob('*.flowmux-backup-*'))
        assert len(backups) == 1 and backups[0].read_text() == 'local custom helper'
        h.pass_check('Update repairs missing support files and preserves modified helper')

        # Execute the installed helper/sample through real panes using a deterministic provider.
        bin_dir = h.root / 'bin'
        bin_dir.mkdir()
        agent = bin_dir / 'claude'
        agent.write_text("""#!/usr/bin/env python3
import json,sys,time,pathlib,os
prompt=sys.argv[-1]
identity=sys.argv[sys.argv.index('--session-id')+1]
p=pathlib.Path(os.environ['CLAUDE_CONFIG_DIR'])/'projects'/'fixture'/(identity+'.jsonl')
p.parent.mkdir(parents=True,exist_ok=True)
p.write_text(json.dumps({'type':'user','sessionId':identity,'message':{'content':prompt}})+'\\n')
print('TOOL  Read README.md',flush=True)
time.sleep(2)
state='blocked' if 'FORCE_BLOCKED' in pathlib.Path('README.md').read_text() else 'completed'
report='Fixture report: '+state
with p.open('a') as f:
 f.write(json.dumps({'type':'assistant','message':{'stop_reason':'end_turn','content':[{'type':'text','text':json.dumps({'task_status':state,'report':report})}]}})+'\\n')
print(report,flush=True)
time.sleep(1)
""")
        agent.chmod(0o700)
        workspace = h.rpc(socket, 'workspace_create', name='Team verification', root=str(h.root), team=True)['workspace_created']['id']
        pane = h.workspace(socket, workspace)['panes'][0]['id']
        alias = h.root / 'selected.sock'
        alias.symlink_to(socket)
        env = dict(h.env, PATH=str(bin_dir) + os.pathsep + h.env['PATH'],
                   FLOWMUX_SOCKET_PATH=str(alias), FLOWMUX_PANE_ID=pane,
                   FLOWMUX_BUNDLED_CLI_PATH=str(Path(args.cli).resolve()), TEAM_SKILL=str(team), TEAM_AGENT='claude', TEAM_REVIEW_AGENT='claude')
        sample = (team / 'references/sample.md').read_text().split("python3 - <<'PY'\n", 1)[1].split('\nPY\n', 1)[0]
        for blocked in (False, True):
            (h.root / 'README.md').write_text('# Flowmux\nCommand: flowmux\n' + ('FORCE_BLOCKED' if blocked else ''))
            result = subprocess.run([sys.executable, '-c', sample], cwd=h.root, env=env,
                                    capture_output=True, text=True, timeout=45)
            (h.root / f'sample-{blocked}.log').write_text(result.stdout + result.stderr)
            assert (result.returncode != 0) == blocked, result.stdout + result.stderr
            artifacts = Path(result.stdout.split('Sample artifacts: ', 1)[1].splitlines()[0])
            receipts = list(artifacts.glob('*.json'))
            assert len(receipts) == (1 if blocked else 2)
            for receipt in receipts:
                job = json.loads(receipt.read_text())
                status = json.loads((Path(job['job']) / 'status.json').read_text())
                assert job['socket'] == str(socket)
                child = next(p for p in h.workspace(socket, workspace)['panes'] if p['id'] == job['pane'])
                assert len(child['tabs']) == 1 and child['tabs'][0]['id'] == job['surface'], child
                assert status['state'] == ('blocked' if blocked else 'completed')
                fixture.wait_for(lambda: 'Fixture report:' in h.screen(socket, job['pane']), 'visible worker report')
        (h.root / 'README.md').write_text('# Flowmux\nCommand: flowmux\n')
        h.pass_check('Installed sample hands results to reviewer; blocked worker stops chain; socket aliases work')

        (team / 'SKILL.md').write_text('custom instructions')
        (team / 'notes.txt').write_text('keep my notes')
        click(button('Remove', row))
        button('Install', row)
        assert all(not (team / name).exists() for name in files)
        assert (team / 'notes.txt').read_text() == 'keep my notes'
        assert browser.exists(), 'Team removal must preserve CLI skill'
        assert any(p.read_text() == 'custom instructions' for p in team.glob('*.flowmux-backup-*'))
        click(button('Install', row))
        button('Installed', row)
        h.pass_check('Remove preserves custom files/backups and other skills; reinstall works')
        click(button('Close', find('Options', Atspi.Role.FRAME)))
        fixture.wait_for(lambda: find('Options', Atspi.Role.FRAME) is None, 'Options closed')
        click(button('Options'))
        click(fixture.wait_for(lambda: find('Skills', Atspi.Role.PAGE_TAB), 'Skills tab'))
        group = fixture.wait_for(lambda: find('Flowmux Team', Atspi.Role.LIST), 'Team group reopened')
        row = find('Claude Code', Atspi.Role.LIST_ITEM, group)
        button('Installed', row)
        scroll_to_team()
        screenshot('skills-reopened.png')
        h.pass_check('Reopened Settings shows persisted installation status')
        click(button('Close', find('Options', Atspi.Role.FRAME)))
        fixture.wait_for(lambda: find('Options', Atspi.Role.FRAME) is None, 'Options closed before workspace menu')
        # Create through the actual sidebar context menu, then restore the persisted type.
        title = fixture.wait_for(lambda: find('Team', Atspi.Role.LABEL), 'workspace title')
        bounds = title.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
        with closing(display.Display(h.env['DISPLAY'])) as connection:
            xtest.fake_input(connection, X.MotionNotify, x=bounds.x + 30, y=bounds.y + 260)
            xtest.fake_input(connection, X.ButtonPress, 3)
            xtest.fake_input(connection, X.ButtonRelease, 3)
            connection.sync()
        before_ids = {w['id'] for w in h.tree(socket)}
        screenshot('team-menu.png')
        click(button('New Team Workspace'))
        gui_team = fixture.wait_for(lambda: next((w for w in h.tree(socket) if w['id'] not in before_ids), None), 'GUI Team workspace')
        assert gui_team['location']['type'] == 'team', gui_team
        fixture.wait_for(lambda: find('Team', Atspi.Role.LABEL), 'Team badge')
        h.pass_check('Sidebar New Team Workspace creates a distinct Team type and visible badge')
        # The shared Codex server may retain the other window's environment.
        def topology(workspaces):
            return [(w['id'], [(p['id'], [(t['id'], t['kind'], t['active']) for t in p['tabs']])
                               for p in w['panes']]) for w in workspaces]

        other_process, other_socket = h.window('other-window')
        other_workspace = h.rpc(other_socket, 'workspace_create', name='Other window', root=str(h.root))['workspace_created']['id']
        other_pane = h.workspace(other_socket, other_workspace)['panes'][0]['id']
        inactive = h.rpc(socket, 'workspace_create', name='Last workspace', root=str(h.root))['workspace_created']['id']
        last_before = h.workspace(socket, inactive)
        other_before = h.tree(other_socket)
        source_before = h.workspace(socket, workspace)
        session = str(uuid.uuid4())
        surface = source_before['panes'][0]['tabs'][0]['id']
        h.rpc(socket, 'agent_activity_update', pane=pane, surface=surface, agent='codex',
              activity='running', session_id=session, source='flowmux:hook')
        task = h.root / 'routing-task.txt'
        task.write_text('Return the fixture report. No changes.')
        receipt = h.root / 'routing-receipt.json'
        errors = h.root / 'routing-errors.log'
        runner = h.root / 'routing-launch.sh'
        command = [sys.executable, str(team / 'scripts/team.py'), 'start', '--agent', 'claude',
                   '--role', 'Window routing fixture', '--cwd', str(h.root), '--task-file', str(task)]
        stale_env = dict(env, FLOWMUX_SOCKET_PATH=str(other_socket), FLOWMUX_PANE_ID=other_pane,
                         CODEX_THREAD_ID=session)
        # Run from the real source pane, but reproduce the daemon's stale context.
        runner.write_text('#!/bin/sh\n' + shlex.join(['env',
            'PATH=' + stale_env['PATH'], 'FLOWMUX_SOCKET_PATH=' + str(other_socket),
            'FLOWMUX_PANE_ID=' + other_pane, 'CODEX_THREAD_ID=' + session,
            'FLOWMUX_BUNDLED_CLI_PATH=' + str(Path(args.cli).resolve()), *command]) +
            ' > ' + shlex.quote(str(receipt)) + ' 2> ' + shlex.quote(str(errors)) + '\n')
        h.send(socket, pane, '/bin/sh ' + shlex.quote(str(runner)))
        job = fixture.wait_for(lambda: json.loads(receipt.read_text()) if receipt.exists() and receipt.stat().st_size else None,
                               'worker launched in originating window')
        fixture.wait_for(lambda: 'TOOL  Read' in h.screen(socket, job['pane']), 'live tool progress')
        assert json.loads((Path(job['job']) / 'status.json').read_text())['state'] == 'running'
        screenshot('team-live-progress.png')
        result = subprocess.run([sys.executable, str(team / 'scripts/team.py'), 'wait', job['job'], '--timeout', '20'],
                                env=env, capture_output=True, text=True, timeout=25)
        assert result.returncode == 0, result.stdout + result.stderr
        assert job['socket'] == str(socket) and job['source_pane'] == pane and job['workspace'] == workspace, job
        assert len(h.workspace(socket, workspace)['panes']) == len(source_before['panes']) + 1
        assert topology([h.workspace(socket, inactive)]) == topology([last_before])
        assert topology(h.tree(other_socket)) == topology(other_before)
        fixture.wait_for(lambda: 'Fixture report:' in h.screen(socket, job['pane']), 'correct window visible worker report')
        h.pass_check('Two windows: stale Codex context resolves to source session, not other window or last workspace')
        screenshot('team-origin-window.png')
        before = [topology(h.tree(socket)), topology(h.tree(other_socket))]
        for extra, identity in [(['--pane', other_pane], session), ([], str(uuid.uuid4()))]:
            result = subprocess.run(command + extra, env=dict(stale_env, CODEX_THREAD_ID=identity),
                                    capture_output=True, text=True, timeout=20)
            assert result.returncode != 0, result.stdout
            assert [topology(h.tree(socket)), topology(h.tree(other_socket))] == before
        h.pass_check('Conflicting pane overrides and unknown sessions create no panes or workspaces')
        # Both the helper and direct IPC reject a regular workspace without mutation.
        before = topology(h.tree(socket))
        rejected = subprocess.run(command, env=dict(env, FLOWMUX_PANE_ID=last_before['panes'][0]['id']),
                                  capture_output=True, text=True, timeout=20)
        assert rejected.returncode != 0 and 'Team workspace' in rejected.stderr, rejected.stderr
        try:
            h.rpc(socket, 'team_spawn', pane=last_before['panes'][0]['id'], cwd=str(h.root), shell='/bin/sh', role='must reject')
        except RuntimeError as error:
            assert 'Team workspace' in str(error), error
        else:
            raise AssertionError('Server accepted worker in regular workspace')
        assert topology(h.tree(socket)) == before
        h.pass_check('Helper and server reject normal workspaces without adding panes')
        h.close_window(other_process)
        h.close_window(process)
        process, socket = h.window('restored-team')
        restored = h.workspace(socket, workspace)
        assert restored['location']['type'] == 'team'
        assert len(restored['panes']) == len(source_before['panes']) + 1
        fixture.wait_for(lambda: 'No task was repeated' in h.screen(socket, job['pane']), 'restored worker does not rerun')
        h.pass_check('Team workspace persists and restoring workers does not rerun tasks')
        h.close_window(process)
        team_lifecycle(h)
    except Exception:
        for path in sorted(h.root.glob('*.log')):
            print(f'--- {path.name} ---\n{path.read_text(errors="replace")[-16000:]}', file=sys.stderr)
        raise
    finally:
        h.cleanup()


if __name__ == '__main__':
    main()
