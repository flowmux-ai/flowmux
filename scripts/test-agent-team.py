#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise team launch, interactive sessions, result validation and routing without an API account."""
import contextlib
import fcntl
import importlib.util
import io
import json
import os
import shlex
import shutil
import signal
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

HELPER = Path(__file__).resolve().parents[1] / '.agents/skills/flowmux-team/scripts/team.py'
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('team', HELPER)
team = importlib.util.module_from_spec(spec)
spec.loader.exec_module(team)


class TeamTests(unittest.TestCase):
    def setUp(self):
        clean = patch.dict(os.environ, {k: v for k, v in os.environ.items()
                           if not k.startswith(('FLOWMUX_', 'CODEX_'))}, clear=True)
        clean.start()
        self.addCleanup(clean.stop)
        self.temporary = tempfile.TemporaryDirectory(prefix="fm-team-test-")
        self.addCleanup(self.temporary.cleanup)
        self.job = Path(self.temporary.name)
        self.socket = self.job / 'window.sock'
        self.socket.touch()
        self.env = dict(os.environ, FLOWMUX_PANE_ID='child', FLOWMUX_SOCKET_PATH=str(self.socket),
                        CODEX_HOME=str(self.job / 'codex-home'), CLAUDE_CONFIG_DIR=str(self.job / 'claude-home'))
        state_env = patch.dict(os.environ, XDG_STATE_HOME=str(self.job / 'state'))
        state_env.start()
        self.addCleanup(state_env.stop)

    def fixture(self, code, agent='codex', timeout=5):
        for attempt in self.job.glob('attempt-*'):
            shutil.rmtree(attempt)
        for name in ('started', 'answer.txt', 'result.md'):
            (self.job / name).unlink(missing_ok=True)
        (self.job / 'task.txt').write_text('Read README, preserve prior edits.')
        for home in ('codex-home', 'claude-home'):
            for transcript in (self.job / home).rglob('*.jsonl'):
                transcript.unlink()
        executable = self.job / agent
        executable.write_text(f'#!{sys.executable}\n' + code)
        executable.chmod(0o700)
        (self.job / 'prompt.txt').write_text('literal task: $(touch NEVER) `false` \\n 한글')
        team.write_json(self.job / 'job.json', {
            'agent': agent, 'executable': str(executable), 'cwd': str(self.job),
            'role': 'reviewer', 'allow_edits': False, 'timeout': timeout,
            'pane': 'child', 'socket': str(self.socket), 'token': 'test-token', 'created_at': time.time(),
        })
        team.write_json(self.job / 'status.json', {'state': 'pending'})

    def run_worker(self):
        return subprocess.run([sys.executable, str(HELPER), '_worker', str(self.job)],
                              env=self.env, capture_output=True, text=True, timeout=10)

    def state(self):
        return json.loads((self.job / 'status.json').read_text())['state']

    def report_code(self, agent, outcome='completed'):
        answer = json.dumps({'task_status': outcome, 'report': 'checked README'})
        if agent == 'codex':
            path = self.job / 'codex-home/sessions/2026/10/09/session.jsonl'
            events = [
                {'type': 'session_meta', 'payload': {'id': 'session'}},
                {'type': 'response_item', 'payload': {'role': 'user', 'content': [{'text': 'FLOWMUX_TEAM_JOB:test-token'}]}},
                {'type': 'event_msg', 'payload': {'type': 'task_complete', 'last_agent_message': answer}},
            ]
        else:
            path = self.job / 'claude-home/projects/project/test-token.jsonl'
            events = [
                {'type': 'user', 'sessionId': 'session', 'message': {'content': 'FLOWMUX_TEAM_JOB:test-token'}},
                {'type': 'assistant', 'message': {'stop_reason': 'end_turn', 'content': [{'type': 'text', 'text': answer}]}},
            ]
        return ("import pathlib,time\n" + f"p=pathlib.Path({str(path)!r})\n" +
                "p.parent.mkdir(parents=True,exist_ok=True)\n" +
                f"p.write_text({''.join(json.dumps(e) + chr(10) for e in events)!r})\n" +
                "print('Native interactive agent',flush=True)\ntime.sleep(1)\n")

    def test_task_outcomes_and_reentry_for_both_interactive_providers(self):
        for agent in ('codex', 'claude'):
            for outcome in ('completed', 'blocked', 'failed'):
                with self.subTest(agent=agent, outcome=outcome):
                    self.fixture(self.report_code(agent, outcome), agent=agent)
                    (self.job / 'started').unlink(missing_ok=True)
                    result = self.run_worker()
                    self.assertEqual(self.state(), outcome, result.stderr)
                    self.assertIn('Native interactive agent', result.stdout)
                    self.assertEqual((self.job / 'result.md').read_text(), 'checked README')
                    with contextlib.redirect_stdout(io.StringIO()):
                        self.assertEqual(team.inspect(self.job), 0 if outcome == 'completed' else 1)
                    self.assertEqual(self.run_worker().returncode, 1)

    def test_error_exit_and_missing_answer_are_not_success(self):
        for code in ('raise SystemExit(7)', 'pass'):
            self.fixture(code)
            (self.job / 'started').unlink(missing_ok=True)
            self.run_worker()
            self.assertEqual(self.state(), 'failed')
            self.assertFalse((self.job / 'result.md').exists())

    def test_transcript_ignores_other_jobs_tool_output_and_intermediate_messages(self):
        path = self.job / 'transcript.jsonl'
        manifest = {'agent': 'codex', 'token': 'mine'}
        completed = {'type': 'event_msg', 'payload': {'type': 'task_complete', 'last_agent_message': 'answer'}}
        tool = {'type': 'response_item', 'payload': {'role': 'tool', 'content': 'FLOWMUX_TEAM_JOB:mine'}}
        path.write_text(json.dumps(tool) + '\n' + json.dumps(completed) + '\n')
        self.assertEqual(team.transcript_report(path, manifest), (False, None, None, None))
        user = {'type': 'event_msg', 'payload': {'item': {'type': 'UserMessage', 'content': [{'text': 'FLOWMUX_TEAM_JOB:mine'}]}}}
        path.write_text(json.dumps(user) + '\n' + '{"partial":')
        self.assertEqual(team.transcript_report(path, manifest), (True, None, None, None))
        path.write_text(json.dumps(user) + '\n' + json.dumps(completed) + '\n')
        self.assertEqual(team.transcript_report(path, manifest), (True, None, 'answer', None))

    def test_malformed_reports_fail_closed(self):
        for answer in ('BLOCKED: no work done', '{}', '[]', 'null',
                       '{"task_status":"unknown","report":"done"}',
                       '{"task_status":"completed","report":42}',
                       '{"task_status":"completed","report":" "}'):
            with self.subTest(answer=answer):
                with self.assertRaises(ValueError):
                    team.task_report(answer)

    def test_claude_thinking_is_not_completion_and_api_errors_are_blocked(self):
        path = self.job / 'claude.jsonl'
        manifest = {'agent': 'claude', 'token': 'mine'}
        user = {'type': 'user', 'sessionId': 's', 'message': {'content': 'FLOWMUX_TEAM_JOB:mine'}}
        thinking = {'type': 'assistant', 'message': {'stop_reason': 'end_turn', 'content': [{'type': 'thinking', 'thinking': 'checking'}]}}
        path.write_text(json.dumps(user) + '\n' + json.dumps(thinking) + '\n')
        self.assertEqual(team.transcript_report(path, manifest), (True, 's', None, None))
        answer = json.dumps({'task_status': 'completed', 'report': 'verified'})
        final = {'type': 'assistant', 'message': {'stop_reason': 'end_turn', 'content': [{'type': 'text', 'text': answer}]}}
        with path.open('a') as out:
            out.write(json.dumps(final) + '\n')
        self.assertEqual(team.transcript_report(path, manifest), (True, 's', answer, None))
        error = {'type': 'assistant', 'isApiErrorMessage': True, 'error': 'rate_limit', 'message': {'stop_reason': 'stop_sequence', 'content': [{'type': 'text', 'text': 'Usage limit reached'}]}}
        path.write_text(json.dumps(user) + '\n' + json.dumps(error) + '\n')
        self.assertEqual(team.task_report(team.transcript_report(path, manifest)[2]), ('blocked', 'Usage limit reached'))

    def test_wrong_pane_or_socket_refuses_to_launch_agent(self):
        self.fixture("from pathlib import Path\nPath('UNEXPECTED').touch()")
        self.env['FLOWMUX_PANE_ID'] = 'source'
        self.run_worker()
        self.assertFalse((self.job / 'UNEXPECTED').exists())
        self.assertEqual(self.state(), 'failed')

    def test_report_timeout_preserves_interactive_session(self):
        self.fixture("import pathlib,time\ntime.sleep(1.5)\npathlib.Path('STILL_ALIVE').touch()\n", timeout=1)
        self.run_worker()
        self.assertEqual(self.state(), 'timed_out')
        self.assertTrue((self.job / 'STILL_ALIVE').exists())

    def test_wait_deadline_does_not_mark_worker_failed(self):
        self.fixture('pass')
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(team.inspect(self.job, .01), 124)
        self.assertEqual(self.state(), 'pending')

    def test_launch_uses_new_pane_and_executable_launcher(self):
        self.fixture('pass')
        alias = self.job / 'selected.sock'
        alias.symlink_to(self.socket)
        other = self.job / 'other.sock'
        other.touch()
        calls = []
        def cli(_cli, _socket, *args):
            self.assertEqual(_socket, str(self.socket))
            calls.append(args)
            if args[0] == 'tree':
                alias.unlink()
                alias.symlink_to(other)
                return {'tree': {'workspaces': [{'id': 'ws', 'location': {'type': 'team'},
                                                 'panes': [{'id': 'source'}]}]}}
            if args[0] == 'team-spawn':
                launcher = Path(args[-1])
                self.assertTrue(os.access(launcher, os.X_OK))
                self.assertIn('_worker', launcher.read_text())
                return {'surface_created': {'pane': 'child', 'id': 'tab'}}
            return 'ok'
        args = type('Args', (), dict(socket=str(alias), pane='source', cwd=self.job,
                    task_file=self.job / 'prompt.txt', role='reviewer', agent='codex',
                    cli='flowmux', allow_edits=False, timeout=5, complexity='standard', model=None, effort=None, no_fallback=False))()
        artifacts = self.job / "job 'quoted' $path"
        artifacts.mkdir()
        with patch.object(team, 'flowmux', side_effect=cli), \
                patch.object(team.shutil, 'which', return_value='/bin/true'), \
                patch.object(team.tempfile, 'mkdtemp', return_value=str(artifacts)), \
                patch.dict(os.environ, CLAUDE_CONFIG_DIR=str(artifacts / "profile 'quoted' $path")), \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(team.start(args), 0)
        self.assertEqual([c[0] for c in calls], ['tree', 'team-spawn'])
        self.assertEqual(calls[1][1], 'source')
        self.assertEqual((artifacts / 'task.txt').read_text().strip(), (self.job / 'prompt.txt').read_text().strip())
        self.assertNotIn('$(touch NEVER)', (artifacts / 'prompt.txt').read_text())
        self.assertIn(str(artifacts / 'task.txt'), (artifacts / 'prompt.txt').read_text())
        self.assertEqual(json.loads((artifacts / 'job.json').read_text())['model'], 'gpt-6.1-sol')
        self.assertEqual(json.loads((artifacts / 'job.json').read_text())['surface'], 'tab')
        self.assertEqual(json.loads((artifacts / 'job.json').read_text())['socket'], str(self.socket))
        manifest = json.loads((artifacts / 'job.json').read_text())
        self.assertEqual(manifest['provider_env']['CODEX_HOME'], str(Path.home() / '.codex'))
        self.assertEqual(manifest['provider_env']['PATH'], os.environ.get('PATH', os.defpath))
        self.assertEqual(manifest['provider_env']['CLAUDE_CONFIG_DIR'], str(artifacts / "profile 'quoted' $path"))
        self.assertEqual(manifest['executables'], {'claude': '/bin/true', 'codex': '/bin/true'})
        # Match the GUI's saved-shell invocation, including the shell after agent exit.
        output = artifacts / 'resumed-env.json'
        probe = ('import json,os,pathlib; pathlib.Path(' + repr(str(output)) + ').write_text('
                 'json.dumps({k:os.environ.get(k) for k in '
                 '["PATH","CODEX_HOME","CLAUDE_CONFIG_DIR","CODEX_THREAD_ID","CLAUDECODE"]}))')
        launcher = artifacts / 'launch.sh'
        command = shlex.join([sys.executable, '-c', probe]) + '; exec ' + shlex.quote(str(launcher)) + ' -l'
        resumed = subprocess.run([str(launcher), '-lic', command], stdin=subprocess.DEVNULL,
                                 capture_output=True, text=True, timeout=5,
                                 env=dict(self.env, PATH='/nonexistent', CODEX_THREAD_ID='stale', CLAUDECODE='1'))
        self.assertEqual(resumed.returncode, 0, resumed.stderr)
        self.assertEqual(json.loads(output.read_text()), {**manifest['provider_env'],
                                                         'CODEX_THREAD_ID': None, 'CLAUDECODE': None})
        self.assertFalse((artifacts / 'started').exists(), 'Resume must not replay the assignment')

    def test_non_team_workspace_is_rejected_before_mutation(self):
        self.fixture('pass')
        args = type('Args', (), dict(socket=str(self.socket), pane='source', cwd=self.job,
                    task_file=self.job / 'prompt.txt', role='reviewer', agent='codex',
                    cli='flowmux', allow_edits=False, timeout=5, complexity='standard', model=None, effort=None, no_fallback=False))()
        for kind in ('local', 'ssh'):
            tree = {'tree': {'workspaces': [{'id': 'ws', 'location': {'type': kind}, 'panes': [{'id': 'source'}]}]}}
            with patch.object(team, 'flowmux', return_value=tree) as rpc, patch.object(team.shutil, 'which', return_value='/bin/true'):
                with self.assertRaisesRegex(ValueError, 'Team workspace'):
                    team.start(args)
                self.assertEqual(rpc.call_count, 1)
                self.assertEqual(rpc.call_args.args[2], 'tree')

    def test_saved_worker_launcher_survives_temporary_directory_cleanup(self):
        transient = self.job / 'temporary'
        transient.mkdir()
        state = self.job / 'state'
        task = self.job / 'assignment.txt'
        task.write_text('Read-only fixture')
        args = type('Args', (), dict(socket=str(self.socket), pane='source', cwd=self.job,
                    task_file=task, role='reviewer', agent='codex', cli='flowmux',
                    allow_edits=False, timeout=5, complexity='standard', model=None,
                    effort=None, no_fallback=False))()
        launchers = []

        def cli(_cli, _socket, *command):
            if command[0] == 'tree':
                return {'tree': {'workspaces': [{'id': 'ws', 'location': {'type': 'team'},
                                                 'panes': [{'id': 'source'}]}]}}
            launchers.append(Path(command[-1]))
            return {'surface_created': {'pane': 'child', 'id': 'tab'}}

        with patch.dict(os.environ, XDG_STATE_HOME=str(state)), \
                patch.object(team.tempfile, 'tempdir', str(transient)), \
                patch.object(team, 'flowmux', side_effect=cli), \
                patch.object(team.shutil, 'which', return_value='/bin/true'), \
                contextlib.redirect_stdout(io.StringIO()):
            team.start(args)
        shutil.rmtree(transient)
        launcher = launchers[0]
        self.assertTrue(launcher.exists(), 'Saved shell disappeared with temporary files')
        self.assertTrue(launcher.is_relative_to(state))
        self.assertEqual(launcher.parent.stat().st_mode & 0o777, 0o700)
        self.assertEqual((launcher.parent / 'worker.py').read_bytes(), HELPER.read_bytes())
        restored = subprocess.run([str(launcher), '-l'], stdin=subprocess.DEVNULL,
                                  capture_output=True, timeout=5)
        self.assertEqual(restored.returncode, 0, restored.stderr)
        self.assertFalse((launcher.parent / 'started').exists())

    def test_session_origin_overrides_stale_environment_and_rejects_redirection(self):
        other = self.job / 'other.sock'
        other.touch()
        args = type('Args', (), dict(socket=None, pane=None))()
        origin = {'socket': str(self.socket), 'pane': 'source'}
        with patch.dict(os.environ, CODEX_THREAD_ID='current-session',
                        FLOWMUX_SOCKET_PATH=str(other), FLOWMUX_PANE_ID='stale'), \
                patch.object(team, 'flowmux', return_value=origin) as rpc:
            self.assertEqual(team.source_context(args, 'flowmux'), (str(self.socket), 'source'))
            rpc.assert_called_once_with('flowmux', str(other), 'identify', '--session', 'current-session')
            args.pane = 'other-pane'
            with self.assertRaisesRegex(ValueError, 'originating pane'):
                team.source_context(args, 'flowmux')
            args.pane = None
            args.socket = str(self.socket)
            self.assertEqual(team.source_context(args, 'flowmux'), (str(self.socket), 'source'))
            rpc.assert_called_with('flowmux', str(self.socket), 'identify', '--session', 'current-session')
            args.socket = str(other)
            with self.assertRaisesRegex(ValueError, 'originating window'):
                team.source_context(args, 'flowmux')
            args.socket = None
            rpc.side_effect = RuntimeError('No unique live Flowmux pane')
            with self.assertRaisesRegex(RuntimeError, 'No unique'):
                team.source_context(args, 'flowmux')

    def test_local_origin_cannot_be_replaced_by_another_pane(self):
        args = type('Args', (), dict(socket=None, pane='replacement'))()
        with patch.dict(os.environ, FLOWMUX_SOCKET_PATH=str(self.socket), FLOWMUX_PANE_ID='source'):
            with self.assertRaisesRegex(ValueError, 'originating pane'):
                team.source_context(args, 'flowmux')

    def test_sample_preserves_launch_diagnostic_and_stops_failed_handoff(self):
        sample = HELPER.parents[1] / 'references/sample.md'
        source = sample.read_text().split("python3 - <<'PY'\n", 1)[1].split('\nPY\n', 1)[0]
        cases = [None, {'state': 'blocked', 'task_status': 'blocked', 'result': 'no access'},
                 {'state': 'failed', 'task_status': 'failed', 'result': 'check failed'},
                 {'state': 'completed', 'result': 'old format'},
                 {'state': 'completed', 'task_status': 'completed', 'result': 'verified'}]
        for report in cases:
            with self.subTest(report=report):
                launches = []
                def run(args, **kwargs):
                    if args[2] == 'start':
                        launches.append(args[args.index('--role') + 1])
                        if report is None:
                            return subprocess.CompletedProcess(args, 1, '', 'Launch unconfirmed: /tmp/job-evidence')
                        return subprocess.CompletedProcess(args, 0, json.dumps({'pane': 'child', 'job': str(self.job)}), '')
                    return subprocess.CompletedProcess(args, 0, json.dumps(report), '')
                with patch.dict(os.environ, TEAM_SKILL=str(HELPER.parents[1]), TEAM_AGENT='codex'), \
                        patch.object(subprocess, 'run', side_effect=run), \
                        patch.object(tempfile, 'mkdtemp', return_value=str(self.job)), \
                        contextlib.redirect_stdout(io.StringIO()):
                    if report and report.get('task_status') == 'completed':
                        (self.job / 'result.md').write_text('verified')
                        exec(compile(source, str(sample), 'exec'), {})
                        self.assertEqual(launches, ['researcher', 'reviewer'])
                    else:
                        with self.assertRaises(RuntimeError) as error:
                            exec(compile(source, str(sample), 'exec'), {})
                        self.assertEqual(launches, ['researcher'])
                        if report is None:
                            self.assertIn('/tmp/job-evidence', str(error.exception))
                            self.assertIn('/tmp/job-evidence', (self.job / 'researcher-launch.log').read_text())

    def test_permission_arguments_are_bounded(self):
        for agent in ('codex', 'claude'):
            for edits in (False, True):
                (self.job / 'prompt.txt').write_text('task')
                manifest = dict(agent=agent, executable=agent, allow_edits=edits, token='test-token')
                args = team.agent_command(self.job, manifest)
                self.assertFalse(any('danger' in arg or 'bypass' in arg for arg in args))
                if agent == 'codex':
                    self.assertIn('workspace-write' if edits else 'read-only', args)
                    self.assertNotIn('exec', args)
                    self.assertIn('--no-daemon', args)
                else:
                    self.assertNotIn('--print', args)
                    self.assertEqual(args[-2:], ['--', 'task'])
                    self.assertEqual(args[args.index('--permission-mode') + 1], 'dontAsk')
                    self.assertNotIn('Bash', args[args.index('--tools') + 1])

    def test_model_routing_and_explicit_selection_reach_native_cli(self):
        for agent in ('codex', 'claude'):
            for complexity in ('simple', 'standard', 'complex'):
                args = type('Args', (), dict(agent=agent, complexity=complexity, model=None, effort=None))()
                route = team.model_route(args)
                self.assertTrue(route['model'])
                (self.job / 'prompt.txt').write_text('task')
                manifest = dict(agent=agent, executable=agent, allow_edits=False, token='t', **route)
                argv = team.agent_command(self.job, manifest)
                self.assertEqual(argv[argv.index('--model') + 1], route['model'])
                self.assertEqual(argv[-2:], ['--', 'task'])
                if agent == 'codex':
                    self.assertIn(f'model_reasoning_effort="{route["effort"]}"', argv)
                else:
                    self.assertEqual(argv[argv.index('--effort') + 1], route['effort'])
                args.model = 'user-model'
                explicit = team.model_route(args)
                self.assertEqual(explicit['model'], 'user-model')
                self.assertIsNone(explicit['effort'])
                args.effort = 'low'
                self.assertEqual(team.model_route(args)['effort'], 'low')

    def quota_fixture(self, agent, outcome):
        executable = self.job / agent
        executable.write_text(f"#!{sys.executable}\n" + f"agent={agent!r}\noutcome={outcome!r}\n" + r"""
import json, os, pathlib, re, signal, sys, time
token = re.search(r'FLOWMUX_TEAM_JOB:([^\s]+)', sys.argv[-1])[1]
root = pathlib.Path.cwd()
primary = token == 'test-token'
if not primary and not (root / 'stopped').exists():
    raise SystemExit('Original worker still running')
(root / (agent + '-argv.json')).write_text(json.dumps(sys.argv))
if agent == 'codex':
    path = pathlib.Path(os.environ['CODEX_HOME']) / 'sessions/2026/10/09' / (token + '.jsonl')
    user = {'type': 'response_item', 'payload': {'role': 'user', 'content': 'FLOWMUX_TEAM_JOB:' + token}}
    if outcome == 'quota':
        final = {'type': 'event_msg', 'payload': {'type': 'task_complete', 'last_agent_message': None,
                 'error': {'codex_error_info': 'usage_limit_exceeded', 'message': 'You’ve hit your usage limit.'}}}
    else:
        final = {'type': 'event_msg', 'payload': {'type': 'task_complete', 'last_agent_message': json.dumps({'task_status': 'completed', 'report': 'checked README'})}}
else:
    path = pathlib.Path(os.environ['CLAUDE_CONFIG_DIR']) / 'projects/project' / (token + '.jsonl')
    user = {'type': 'user', 'sessionId': token, 'message': {'content': 'FLOWMUX_TEAM_JOB:' + token}}
    final = {'type': 'assistant', 'message': {'stop_reason': 'end_turn', 'content': [{'type': 'text', 'text': "You've hit your session limit" if outcome == 'quota' else json.dumps({'task_status': 'completed', 'report': 'checked README'})}]}}
    if outcome == 'quota':
        final.update(isApiErrorMessage=True, error='rate_limit')
def stop(*_):
    (root / 'stopped').touch()
    sys.exit(0)
if primary:
    signal.signal(signal.SIGTERM, stop)
path.parent.mkdir(parents=True, exist_ok=True)
path.write_text(json.dumps(user) + '\n' + json.dumps(final) + '\n')
time.sleep(30 if primary else .8)
""")
        executable.chmod(0o700)

    def test_quota_fallback_both_directions_and_two_attempt_limit(self):
        for agent in ('codex', 'claude'):
            for outcome in ('completed', 'quota'):
                with self.subTest(agent=agent, replacement=outcome):
                    self.fixture('pass', agent=agent)
                    (self.job / 'stopped').unlink(missing_ok=True)
                    manifest = json.loads((self.job / 'job.json').read_text())
                    other = 'claude' if agent == 'codex' else 'codex'
                    manifest.update(fallback_on_quota=True, complexity='simple',
                                    provider_env={key: str(self.job / home) for key, home in
                                                  [('CODEX_HOME', 'codex-home'), ('CLAUDE_CONFIG_DIR', 'claude-home')]},
                                    executables={name: str(self.job / name) for name in ('claude', 'codex')})
                    team.write_json(self.job / 'job.json', manifest)
                    (self.job / 'prompt.txt').write_text(team.worker_prompt(self.job, manifest))
                    self.quota_fixture(agent, 'quota')
                    self.quota_fixture(other, outcome)
                    # PATH-dispatching shims cannot launch either provider from the GUI's PATH.
                    native = self.job / 'lead-bin'
                    native.mkdir(exist_ok=True)
                    for name in ('codex', 'claude'):
                        executable = self.job / name
                        executable.replace(native / ('fixture-' + name))
                        executable.write_text('#!/bin/sh\nexec fixture-' + name + ' "$@"\n')
                        executable.chmod(0o700)
                    manifest['provider_env']['PATH'] = str(native) + os.pathsep + os.defpath
                    team.write_json(self.job / 'job.json', manifest)
                    # Both attempts must use the lead's roots/binaries, not the GUI's environment.
                    self.env.update(CODEX_HOME=str(self.job / 'wrong-codex'),
                                    CLAUDE_CONFIG_DIR=str(self.job / 'wrong-claude'), PATH='/nonexistent')
                    result = self.run_worker()
                    status = json.loads((self.job / 'status.json').read_text())
                    self.assertEqual(status['state'], 'completed' if outcome == 'completed' else 'blocked', result.stderr)
                    self.assertTrue(status['fallback_used'])
                    self.assertEqual(len(status['attempts']), 2)
                    self.assertTrue(status['attempts'][0]['stopped_for_fallback'])
                    self.assertEqual(status['agent'], other)
                    self.assertEqual(status['model'], team.MODEL_ROUTES[other]['simple'][0])
                    alternate = json.loads((self.job / 'attempt-2/job.json').read_text())
                    self.assertNotEqual(alternate['token'], manifest['token'])
                    self.assertEqual(alternate['pane'], manifest['pane'])
                    self.assertEqual(alternate['socket'], manifest['socket'])
                    self.assertEqual(alternate['allow_edits'], manifest['allow_edits'])
                    self.assertIn(str(self.job / 'task.txt'), (self.job / 'attempt-2/task.txt').read_text())
                    self.assertEqual(json.loads((self.job / 'job.json').read_text())['agent'], agent)
                    self.assertEqual(json.loads((self.job / 'attempt-1/status.json').read_text())['blocker_kind'], 'quota')

    def test_interrupted_supervisor_is_reconciled_without_replay(self):
        for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGKILL):
            with self.subTest(signal=sig):
                self.fixture('import time\ntime.sleep(30)', timeout=20)
                process = subprocess.Popen([sys.executable, str(HELPER), '_worker', str(self.job)],
                                           env=self.env, stdout=subprocess.DEVNULL,
                                           stderr=subprocess.DEVNULL, start_new_session=True)
                try:
                    deadline = time.monotonic() + 5
                    while self.state() != 'running' and time.monotonic() < deadline:
                        time.sleep(.02)
                    self.assertEqual(self.state(), 'running')
                    with contextlib.redirect_stdout(io.StringIO()):
                        self.assertEqual(team.inspect(self.job, .01), 124)
                    self.assertEqual(self.run_worker().returncode, 1)
                    self.assertEqual(self.state(), 'running', 'A live owner must not be interrupted')
                    process.send_signal(sig)
                    process.wait(timeout=3)
                    # Child remains alive: only the collector owns the lock, not its descendants.
                    if sig == signal.SIGHUP:
                        self.assertEqual(self.run_worker().returncode, 1)
                    readers = [subprocess.Popen([sys.executable, str(HELPER), 'wait', str(self.job), '--timeout', '1'],
                                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                               for _ in range(3)]
                    for reader in readers:
                        stdout, stderr = reader.communicate(timeout=3)
                        self.assertEqual(reader.returncode, 1, stderr)
                        self.assertEqual(json.loads(stdout)['blocker_kind'], 'interrupted')
                    self.assertEqual(self.state(), 'failed')
                    self.assertEqual(len(list(self.job.glob('attempt-*'))), 1)
                finally:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=3)

    def test_legacy_jobs_and_final_reports_are_not_reclassified(self):
        self.fixture('pass')
        (self.job / 'started').touch()  # Old helpers did not acquire worker.lock.
        team.write_json(self.job / 'status.json', {'state': 'running'})
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(team.inspect(self.job, .01), 124)
        (self.job / 'started').write_text('lock-v1\n')
        for outcome in team.TERMINAL:
            team.write_json(self.job / 'status.json', {'state': outcome})
            before = (self.job / 'status.json').read_bytes()
            with contextlib.redirect_stdout(io.StringIO()):
                team.inspect(self.job)
            self.assertEqual((self.job / 'status.json').read_bytes(), before)

    def test_status_reader_cannot_prevent_worker_startup(self):
        self.fixture(self.report_code('codex'))
        with (self.job / 'worker.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            process = subprocess.Popen([sys.executable, str(HELPER), '_worker', str(self.job)],
                                       env=self.env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                time.sleep(.2)
                self.assertIsNone(process.poll(), 'Initial inspection must not reject worker startup')
            finally:
                fcntl.flock(lock, fcntl.LOCK_UN)
                process.wait(timeout=5)
        self.assertEqual(self.state(), 'completed')

    def test_only_authoritative_quota_triggers_fallback(self):
        for code, message, expected in (
            ('UsageLimitExceeded', 'limit', True),
            ('usage_limit_exceeded', 'limit', True),
            ('usage_limit_reached', 'limit', True),
            ('rate_limit', 'You’ve hit your usage limit.', True),
            ('rate_limit', 'You’ve hit your session limit.', True),
            ('insufficient_quota', 'limit', True),
            ('rate_limit', "You've hit your session limit", True),
            ('rate_limit', 'Too many requests', False),
            ('authentication_error', 'insufficient credits', False),
            ('permission_denied', 'Usage limit reached', False),
            ('server_error', 'temporarily unavailable', False),
            ('server_overloaded', 'Selected model is at capacity.', False),
        ):
            self.assertEqual(team.quota_error(code, message), expected)
        path = self.job / 'native.jsonl'
        for agent in ('claude', 'codex'):
            if agent == 'codex':
                user = {'type': 'response_item', 'payload': {'role': 'user', 'content': 'FLOWMUX_TEAM_JOB:mine'}}
                final = {'type': 'event_msg', 'payload': {'type': 'task_complete', 'last_agent_message': 'Usage limit reached'}}
            else:
                user = {'type': 'user', 'message': {'content': 'FLOWMUX_TEAM_JOB:mine'}}
                final = {'type': 'assistant', 'message': {'stop_reason': 'end_turn', 'content': [{'type': 'text', 'text': 'Usage limit reached'}]}}
            path.write_text(json.dumps(user) + '\n' + json.dumps(final))
            self.assertIsNone(team.transcript_report(path, {'agent': agent, 'token': 'mine'})[3])
        retry = {'type': 'event_msg', 'payload': {'type': 'error', 'will_retry': True, 'message': 'Usage limit reached'}}
        user = {'type': 'response_item', 'payload': {'role': 'user', 'content': 'FLOWMUX_TEAM_JOB:mine'}}
        path.write_text(json.dumps(user) + '\n' + json.dumps(retry))
        self.assertIsNone(team.transcript_report(path, {'agent': 'codex', 'token': 'mine'})[2])

    def test_codex_native_errors_in_standalone_and_completion_events(self):
        path = self.job / 'native.jsonl'
        user = {'type': 'event_msg', 'payload': {'type': 'user_message', 'message': 'FLOWMUX_TEAM_JOB:mine'}}
        manifest = {'agent': 'codex', 'token': 'mine'}
        for nested in (False, True):
            for code, message, kind in (
                ('usage_limit_exceeded', 'You’ve hit your usage limit.', 'quota'),
                ('server_overloaded', 'Selected model is at capacity.', 'provider_error'),
                ('permission_denied', 'Usage limit reached', 'provider_error'),
            ):
                with self.subTest(nested=nested, code=code):
                    error = {'codex_error_info': code, 'message': message}
                    payload = ({'type': 'task_complete', 'last_agent_message': None, 'error': error}
                               if nested else {'type': 'error', **error})
                    event = {'type': 'event_msg', 'payload': payload}
                    path.write_text(json.dumps(event) + '\n')
                    self.assertEqual(team.transcript_report(path, manifest), (False, None, None, None))
                    path.write_text(json.dumps(user) + '\n' + json.dumps(event) + '\n')
                    matched, _, answer, blocker = team.transcript_report(path, manifest)
                    self.assertTrue(matched)
                    self.assertEqual(blocker, kind)
                    self.assertEqual(team.task_report(answer), ('blocked', message))

    def test_disabled_missing_or_unstoppable_fallback_does_not_launch(self):
        for case in ('disabled', 'missing', 'unstoppable', 'permission', 'timeout'):
            with self.subTest(case=case):
                self.fixture('pass')
                manifest = json.loads((self.job / 'job.json').read_text())
                manifest['fallback_on_quota'] = case != 'disabled'
                team.write_json(self.job / 'job.json', manifest)
                child = unittest.mock.Mock()
                child.poll.return_value = None
                if case == 'unstoppable':
                    child.wait.side_effect = [subprocess.TimeoutExpired('agent', 5), 0]
                state = {'state': 'timed_out' if case == 'timeout' else 'blocked',
                         'blocker_kind': 'quota' if case not in ('permission', 'timeout') else 'provider_error'}
                with patch.dict(os.environ, self.env, clear=True), \
                     patch.object(team, 'run_attempt', return_value=(child, state)) as run, \
                     patch.object(team.shutil, 'which', return_value=None if case == 'missing' else '/other'), \
                     contextlib.redirect_stdout(io.StringIO()):
                    team.worker(self.job)
                self.assertEqual(run.call_count, 1)
                self.assertFalse(json.loads((self.job / 'status.json').read_text())['fallback_used'])
                if case != 'unstoppable':
                    child.terminate.assert_not_called()

    def test_cli_error_preserves_diagnostic(self):
        result = subprocess.CompletedProcess([], 1, '', 'pane not found')
        with patch.object(team.subprocess, 'run', return_value=result):
            with self.assertRaisesRegex(RuntimeError, 'pane not found'):
                team.flowmux('flowmux', '/test.sock', 'split', 'missing')


if __name__ == '__main__':
    unittest.main()
