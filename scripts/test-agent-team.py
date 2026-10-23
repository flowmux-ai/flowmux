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
        # Match start()'s canonical socket paths, including macOS /tmp aliases.
        self.job = Path(self.temporary.name).resolve()
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
        for answer in (None, 42, {}, [], 'BLOCKED: no work done', '{}', '[]', 'null',
                       '{"task_status":"unknown","report":"done"}',
                       '{"task_status":"completed","report":42}',
                       '{"task_status":"completed","report":" "}'):
            with self.subTest(answer=answer):
                with self.assertRaises(ValueError):
                    team.task_report(answer)

    def test_task_report_accepts_only_bare_or_single_fenced_json(self):
        for outcome in ('completed', 'blocked', 'failed'):
            answer = json.dumps({'task_status': outcome, 'report': 'checked README'})
            for wrapper in ('{}', '```json\n{}\n```', '```\n{}\n```'):
                with self.subTest(outcome=outcome, wrapper=wrapper):
                    self.assertEqual(team.task_report(' \n' + wrapper.format(answer) + '\n'),
                                     (outcome, 'checked README'))
        for answer in ('Explanation\n```json\n{}\n```', '```json\n{}\n```\nExplanation',
                       '```json\n{}\n```\n```json\n{}\n```', '```json\n{}', '```python\n{}\n```'):
            with self.subTest(answer=answer):
                with self.assertRaises(ValueError):
                    team.task_report(answer.replace('{}', '{"task_status":"completed","report":"done"}'))
        for answer in ('{}', '{"task_status":"unknown","report":"done"}',
                       '{"task_status":"completed","report":" "}'):
            with self.subTest(answer=answer):
                with self.assertRaises(ValueError):
                    team.task_report('```json\n' + answer + '\n```')

    def test_first_and_followup_turns_reject_intervening_user_prompts(self):
        path = self.job / 'boundary.jsonl'
        for agent in ('codex', 'claude'):
            user = (lambda text: {'type': 'event_msg', 'payload': {'type': 'user_message', 'message': text}}) if agent == 'codex' else (
                lambda text: {'type': 'user', 'sessionId': 'session', 'message': {'content': text}})
            answer = json.dumps({'task_status': 'completed', 'report': 'tracked task done'})
            final = {'type': 'event_msg', 'payload': {'type': 'task_complete', 'last_agent_message': answer}} if agent == 'codex' else {
                'type': 'assistant', 'message': {'stop_reason': 'end_turn', 'content': [{'type': 'text', 'text': answer}]}}
            for prefix in ('', json.dumps(user('an earlier task')) + '\n'):
                manifest = {'agent': agent, 'token': 'tracked', 'transcript_offset': len(prefix.encode())}
                for interrupted in (False, True):
                    with self.subTest(agent=agent, offset=manifest['transcript_offset'], interrupted=interrupted):
                        events = [user('FLOWMUX_TEAM_JOB:tracked')]
                        if interrupted:
                            events.append(user('unrelated manual task'))
                        events.append(final)
                        path.write_text(prefix + ''.join(json.dumps(event) + '\n' for event in events))
                        if interrupted:
                            with self.assertRaisesRegex(ValueError, 'interrupted'):
                                team.transcript_report(path, manifest)
                        else:
                            self.assertEqual(team.transcript_report(path, manifest)[2], answer)

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

    def test_unbound_worker_reports_observation_gap_until_session_is_bound(self):
        self.fixture('pass')
        manifest = json.loads((self.job / 'job.json').read_text())
        team.write_json(self.job / 'job.json', {**manifest, 'cli': 'flowmux'})
        with patch.object(team, 'worker_tab', return_value={'agent': {'activity': 'idle'}}) as observe:
            for state in ('pending', 'running'):
                team.write_json(self.job / 'status.json', {'state': state})
                for timeout, expected_exit in ((0, 0), (.01, 124)):
                    with contextlib.redirect_stdout(io.StringIO()) as out:
                        self.assertEqual(team.inspect(self.job, timeout), expected_exit)
                    report = json.loads(out.getvalue())
                    self.assertEqual(report['state'], state)
                    self.assertIn('No worker session is bound yet', report['observation_error'])
                    self.assertNotIn('input_required', report)
                    self.assertNotIn('task_status', report)
                    self.assertEqual(json.loads((self.job / 'status.json').read_text()), {'state': state})
            observe.assert_not_called()
            self.assertFalse((self.job / 'input-required.json').exists())
            team.write_json(self.job / 'status.json', {'state': 'running', 'session_id': 'session'})
            with contextlib.redirect_stdout(io.StringIO()) as out:
                self.assertEqual(team.inspect(self.job), 0)
            self.assertNotIn('observation_error', json.loads(out.getvalue()))
            observe.assert_called_once()

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
        self.assertEqual(manifest['executables'], {agent: '/bin/true' for agent in team.MODEL_ROUTES})
        # Match the GUI's saved-shell invocation, including the shell after agent exit.
        output = artifacts / 'resumed-env.json'
        probe = ('import json,os,pathlib; pathlib.Path(' + repr(str(output)) + ').write_text('
                 'json.dumps({k:os.environ.get(k) for k in '
                 '["PATH","CODEX_HOME","CLAUDE_CONFIG_DIR","GEMINI_CLI_HOME","CODEX_THREAD_ID","CLAUDECODE"]}))')
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

    def test_workers_launch_and_resume_in_yolo_mode(self):
        for agent in ('codex', 'claude'):
            for edits in (False, True):
                for resume in (False, True):
                    with self.subTest(agent=agent, edits=edits, resume=resume):
                        (self.job / 'prompt.txt').write_text('task')
                        manifest = dict(agent=agent, executable=agent, allow_edits=edits,
                                        token='test-token', role='worker')
                        args = team.agent_command(self.job, manifest, resume=resume)
                        prompt = team.worker_prompt(self.job, manifest)
                        self.assertIn('Edit only assigned files.' if edits else
                                      'Read-only; do not change project files.', prompt)
                        if resume:
                            self.assertNotIn('task', args)
                            self.assertNotIn('--session-id', args)
                        else:
                            self.assertEqual(args[-2:], ['--', 'task'])
                        if agent == 'codex':
                            self.assertIn('--dangerously-bypass-approvals-and-sandbox', args)
                            self.assertNotIn('--sandbox', args)
                            self.assertNotIn('exec', args)
                            self.assertIn('--no-daemon', args)
                        else:
                            self.assertIn('--allow-dangerously-skip-permissions', args)
                            self.assertEqual(args[args.index('--permission-mode') + 1], 'bypassPermissions')
                            for flag in ('--print', '--tools', '--allowedTools', '--strict-mcp-config', '--mcp-config'):
                                self.assertNotIn(flag, args)

    def test_model_routing_and_explicit_selection_reach_native_cli(self):
        for agent in ('codex', 'claude'):
            for complexity in ('simple', 'standard', 'complex'):
                args = type('Args', (), dict(agent=agent, complexity=complexity, model=None, effort=None))()
                route = team.model_route(args)
                self.assertTrue(route['model'])
                (self.job / 'prompt.txt').write_text('task')
                manifest = dict(executable=agent, allow_edits=False, token='t', **route)
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
                    for name in ('claude', 'codex'):
                        argv = json.loads((self.job / (name + '-argv.json')).read_text())
                        if name == 'codex':
                            self.assertIn('--dangerously-bypass-approvals-and-sandbox', argv)
                        else:
                            self.assertIn('--allow-dangerously-skip-permissions', argv)
                            self.assertEqual(argv[argv.index('--permission-mode') + 1], 'bypassPermissions')
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
                ('permission_denied', 'Usage limit reached', 'input_required'),
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


class FollowupTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='fm-turn-test-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.transcript = self.root / 'session.jsonl'
        self.transcript.write_text('old completed turn\n')
        self.manifest = dict(agent='codex', token='original', model='fixture', effort='high', role='reviewer',
                             allow_edits=False, workspace='team', pane='worker', surface='tab',
                             socket='/fixture.sock', source_pane='lead', cli=sys.executable,
                             session_id='session', transcript=str(self.transcript))
        team.write_json(self.root / 'job.json', self.manifest)
        team.write_json(self.root / 'status.json', dict(state='completed', task_status='completed',
                        session_id='session', transcript=str(self.transcript)))
        (self.root / 'result.md').write_text('original result')
        self.task = self.root / 'next.txt'
        self.task.write_text('Check the changed requirement')
        self.args = type('Args', (), dict(job=self.root, task_file=self.task, cli=sys.executable))()
        self.tab = dict(id='tab', active=True, kind='terminal', agent=dict(
            name='codex', session_id='session', activity='idle', status='idle'))
        self.calls = []
        self.pasted = ''
        patcher = patch.object(team, 'flowmux', side_effect=self.rpc)
        patcher.start()
        self.addCleanup(patcher.stop)
        self.origin = dict(socket='/fixture.sock', workspace='team', pane='lead', mode='team')
        origin = patch.object(team, 'context', return_value=self.origin)
        origin.start()
        self.addCleanup(origin.stop)

    def managed(self, **overrides):
        manifest = dict(self.manifest, generation=1, timeout=0, stall_timeout=180, timeout_policy='progress')
        manifest.update(overrides)
        team.write_json(self.root / 'job.json', manifest)
        self.owner = (self.root / 'worker.lock').open('a')
        fcntl.flock(self.owner, fcntl.LOCK_EX)
        self.addCleanup(self.owner.close)
        team.write_json(self.root / 'supervisor.json', dict(pid=os.getpid(),
                        started=team.process_snapshot()[os.getpid()][2], generation=1))
        return manifest

    def rpc(self, cli, socket, *args):
        self.calls.append(args)
        if args[0] == 'tree':
            return {'tree': {'workspaces': [dict(id='team', location={'type': 'team'},
                    panes=[dict(id='worker', tabs=[self.tab])])]}}
        if args[0] == 'send-keys':
            self.pasted = args[-1]
        if args[0] == 'read-screen':
            return {'screen_contents': {'text': self.pasted}}
        return {}

    def dispatch(self):
        with contextlib.redirect_stdout(io.StringIO()) as out:
            team.followup(self.args)
        return Path(json.loads(out.getvalue())['job'])

    def answer(self, turn, outcome='completed', report='new evidence'):
        manifest = json.loads((turn / 'job.json').read_text())
        events = [dict(type='event_msg', payload=dict(type='user_message',
                       message='FLOWMUX_TEAM_JOB:' + manifest['token'])),
                  dict(type='event_msg', payload=dict(type='task_complete',
                       last_agent_message=json.dumps(dict(task_status=outcome, report=report))))]
        with self.transcript.open('a') as out:
            out.write(''.join(json.dumps(e) + '\n' for e in events))

    def test_three_turns_preserve_session_and_original_result(self):
        for outcome in ('blocked', 'failed', 'completed'):
            turn = self.dispatch()
            self.assertEqual(team.collect_turn(turn)['state'], 'dispatch_unknown')
            with self.assertRaisesRegex(ValueError, 'Previous assignment'):
                self.dispatch()
            self.answer(turn, outcome)
            self.assertEqual(team.collect_turn(turn)['task_status'], outcome)
            self.assertEqual((turn / 'result.md').read_text(), 'new evidence')
            self.assertEqual(json.loads((turn / 'job.json').read_text())['session_id'], 'session')
        self.assertEqual((self.root / 'result.md').read_text(), 'original result')
        self.assertEqual(len(list((self.root / 'turns').iterdir())), 3)
        self.assertEqual(sum(c[0] == 'send-key' for c in self.calls), 3)

    def test_other_lead_workspace_or_window_cannot_followup(self):
        for key in ('pane', 'workspace', 'socket', 'mode'):
            with self.subTest(key=key), patch.dict(self.origin, {key: 'other'}):
                with self.assertRaisesRegex(ValueError, 'original lead pane'):
                    self.dispatch()
        self.assertFalse(self.calls)
        self.assertFalse((self.root / 'turns').exists())

    def test_input_handoff_is_observed_without_completing_or_sending(self):
        turn = self.dispatch()
        self.calls.clear()
        with patch.dict(self.tab['agent'], activity='needs_input', status='blocked', message='Approval needed'):
            with contextlib.redirect_stdout(io.StringIO()) as out:
                self.assertEqual(team.inspect(turn, timeout=30), 2)
            report = json.loads(out.getvalue())
            self.assertEqual(report['state'], 'waiting_input')
            self.assertEqual(report['input_required']['pane'], 'worker')
            self.assertEqual(report['input_required']['source_pane'], 'lead')
            self.assertNotIn('task_status', report)
            self.assertTrue((turn / 'input-required.json').exists())
            self.assertEqual(json.loads((turn / 'status.json').read_text())['state'], 'dispatch_unknown')
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(team.inspect(turn, timeout=.01), 124)
        self.answer(turn)
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(team.inspect(turn), 0)
        self.assertFalse(any(c[0].startswith('send-') for c in self.calls))

    def test_unrelated_session_input_does_not_request_approval(self):
        turn = self.dispatch()
        with patch.dict(self.tab['agent'], session_id='other', activity='needs_input', status='blocked'):
            with contextlib.redirect_stdout(io.StringIO()) as out:
                self.assertEqual(team.inspect(turn), 0)
            self.assertIn('observation_error', json.loads(out.getvalue()))
            self.assertNotIn('input_required', json.loads(out.getvalue()))

    def test_missing_native_binding_can_report_ui_but_cannot_send(self):
        turn = self.dispatch()
        with patch.dict(self.tab['agent'], session_id=None, activity='needs_input', status='blocked'):
            with contextlib.redirect_stdout(io.StringIO()) as out:
                self.assertEqual(team.inspect(turn), 2)
            self.assertFalse(json.loads(out.getvalue())['input_required']['session_verified'])
            with self.assertRaisesRegex(ValueError, 'does not match'):
                team.idle_worker(sys.executable, self.manifest)

    def test_busy_inactive_replaced_and_shell_targets_receive_no_input(self):
        for field, value in [('active', False), ('kind', 'browser'), ('agent', {}),
                             ('agent', dict(self.tab['agent'], session_id='other')),
                             ('agent', dict(self.tab['agent'], activity='running', status='working')),
                             ('agent', dict(self.tab['agent'], activity='needs_input', status='blocked'))]:
            with self.subTest(field=field, value=value), patch.dict(self.tab, {field: value}):
                with self.assertRaises(ValueError):
                    self.dispatch()
        self.assertFalse(any(c[0].startswith('send-') for c in self.calls))
        self.assertFalse((self.root / 'turns').exists())

    def test_uncertain_enter_is_not_retried_and_late_answer_recovers(self):
        original = self.rpc
        def fail(cli, socket, *args):
            if args[0] == 'send-key':
                raise RuntimeError('IPC timeout after delivery')
            return original(cli, socket, *args)
        with patch.object(team, 'flowmux', side_effect=fail):
            with self.assertRaisesRegex(RuntimeError, 'Do not resend'):
                self.dispatch()
        turn = self.root / 'turns/0001'
        with self.assertRaisesRegex(ValueError, 'Previous assignment'):
            self.dispatch()
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(team.inspect(turn, timeout=0.01), 124)
        self.answer(turn)
        self.assertEqual(team.collect_turn(turn)['state'], 'completed')
        self.assertEqual(self.dispatch().name, '0002')

    def test_managed_uncertain_enter_is_monitored_without_resending(self):
        self.managed(timeout=5, stall_timeout=5)
        sent = []
        def uncertain(cli, socket, *args):
            if args[0] == 'send-key':
                sent.append(args)
                turn = Path(json.loads((self.root / 'active-turn.json').read_text())['job'])
                self.answer(turn)
                raise RuntimeError('IPC timeout after delivery')
            return self.rpc(cli, socket, *args)
        with patch.object(team, 'flowmux', side_effect=uncertain):
            with self.assertRaisesRegex(RuntimeError, 'Do not resend'):
                self.dispatch()
        turn = self.root / 'turns/0001'
        attempt = turn / 'attempt-1'
        attempt.mkdir()
        process = type('Process', (), dict(pid=123))()
        with patch.object(team, 'owned_processes', return_value={}):
            state = team.monitor_managed(turn, attempt, json.loads((turn / 'job.json').read_text()), process, {}, False)
        self.assertEqual(state['state'], 'completed')
        self.assertTrue(state['received'])
        self.assertEqual(len(sent), 1)
        self.assertTrue((turn / 'dispatch-error.json').exists())

    def test_dead_supervisor_rejects_all_providers_before_input_or_turn_creation(self):
        for agent in ('codex', 'claude', 'gemini', 'agy'):
            with self.subTest(agent=agent):
                self.managed(agent=agent)
                self.owner.close()
                with self.assertRaisesRegex(ValueError, 'supervisor is no longer live'):
                    self.dispatch()
                self.assertFalse(self.calls)
                self.assertFalse((self.root / 'turns').exists())

    def test_held_lock_cannot_mask_changed_supervisor_identity(self):
        self.managed()
        saved = json.loads((self.root / 'supervisor.json').read_text())
        for changed in (dict(saved, started='reused pid'), dict(saved, generation=2), dict(saved, pid=-1)):
            team.write_json(self.root / 'supervisor.json', changed)
            with self.assertRaisesRegex(ValueError, 'supervisor identity'):
                self.dispatch()
        (self.root / 'supervisor.json').unlink()
        with self.assertRaisesRegex(ValueError, 'supervisor identity is missing'):
            self.dispatch()
        self.assertFalse(self.calls)

    def test_supervisor_exit_after_paste_prevents_enter(self):
        self.managed()
        def interrupted(cli, socket, *args):
            result = self.rpc(cli, socket, *args)
            if args[0] == 'send-keys':
                self.owner.close()
            return result
        with patch.object(team, 'flowmux', side_effect=interrupted):
            with self.assertRaisesRegex(RuntimeError, 'supervisor is no longer live'):
                self.dispatch()
        self.assertFalse(any(call[0] == 'send-key' for call in self.calls))
        self.assertFalse((self.root / 'active-turn.json').exists())

    def test_followup_budget_overrides_are_recorded_without_changing_previous_assignment(self):
        original = self.managed(timeout=300)
        self.args.timeout, self.args.stall_timeout = 0, 900
        turn = self.dispatch()
        updated = json.loads((turn / 'job.json').read_text())
        self.assertEqual((updated['timeout'], updated['stall_timeout']), (0, 900))
        self.assertEqual(json.loads((self.root / 'job.json').read_text()), original)
        self.assertEqual((self.root / 'result.md').read_text(), 'original result')

    def test_old_copied_helper_cannot_silently_ignore_new_timeout_policy(self):
        self.args.timeout = 0
        with self.assertRaisesRegex(ValueError, 'updated helper'):
            self.dispatch()
        self.assertFalse(self.calls)

    def test_malformed_or_interrupted_turn_cannot_authorize_another_send(self):
        turn = self.dispatch()
        self.answer(turn, report='')
        self.assertEqual(team.collect_turn(turn)['state'], 'failed')
        with self.assertRaisesRegex(ValueError, 'Previous assignment'):
            self.dispatch()

    def test_dispatch_lock_prevents_duplicate_turn(self):
        with (self.root / 'followup.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(ValueError, 'Another followup'):
                self.dispatch()
        self.assertFalse(self.calls)

    def test_followup_uses_effective_provider_after_fallback(self):
        team.write_json(self.root / 'job.json', dict(self.manifest, agent='claude'))
        state = json.loads((self.root / 'status.json').read_text())
        team.write_json(self.root / 'status.json', dict(state, agent='codex', fallback_used=True))
        turn = self.dispatch()
        self.assertEqual(json.loads((turn / 'job.json').read_text())['agent'], 'codex')

    def test_missing_session_on_both_sides_is_not_an_identity_match(self):
        state = json.loads((self.root / 'status.json').read_text())
        team.write_json(self.root / 'status.json', dict(state, session_id=None))
        self.tab['agent'].pop('session_id')
        with self.assertRaisesRegex(ValueError, 'no session identity'):
            self.dispatch()
        self.assertFalse(self.calls)

    def test_claude_tool_results_are_not_prompts_or_completion(self):
        path = self.root / 'claude.jsonl'
        marker = 'FLOWMUX_TEAM_JOB:next'
        manifest = dict(agent='claude', token='next', session_id='same')
        tool = dict(type='user', sessionId='same', message=dict(content=[dict(type='tool_result', content=marker)]))
        final = dict(type='assistant', message=dict(stop_reason='end_turn', content=[dict(type='text', text='not our answer')]))
        path.write_text(json.dumps(tool) + '\n' + json.dumps(final) + '\n')
        self.assertFalse(team.transcript_report(path, manifest)[0])
        user = dict(type='user', sessionId='same', message=dict(content=[dict(type='text', text=marker)]))
        path.write_text(json.dumps(user) + '\n' + json.dumps(tool) + '\n' + json.dumps(final) + '\n')
        self.assertEqual(team.transcript_report(path, manifest)[2], 'not our answer')
        user['sessionId'] = 'different'
        path.write_text(json.dumps(user) + '\n')
        with self.assertRaisesRegex(ValueError, 'session changed'):
            team.transcript_report(path, manifest)

    def test_intervening_user_prompt_and_truncated_transcript_fail_closed(self):
        turn = self.dispatch()
        manifest = json.loads((turn / 'job.json').read_text())
        with self.transcript.open('a') as out:
            for message in ('FLOWMUX_TEAM_JOB:' + manifest['token'], 'unrelated manual task'):
                out.write(json.dumps(dict(type='event_msg', payload=dict(type='user_message', message=message))) + '\n')
        self.assertIn('interrupted', team.collect_turn(turn)['error'])
        self.transcript.write_text('')
        with self.assertRaisesRegex(ValueError, 'truncated'):
            team.transcript_report(self.transcript, manifest)


class ManagedTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='fm-managed-test-')
        self.addCleanup(self.temporary.cleanup)
        self.job = Path(self.temporary.name).resolve()
        environment = patch.dict(os.environ, XDG_STATE_HOME=str(self.job / 'state'))
        environment.start()
        self.addCleanup(environment.stop)

    def monitor_clock(self, *, timeout=0, progress=True, waiting=None, complete_at=400):
        attempt = self.job / 'attempt-1'
        attempt.mkdir()
        path = self.job / 'native.jsonl'
        path.touch()
        team.write_json(self.job / 'status.json', dict(state='pending'))
        manifest = dict(agent='codex', model='chosen', effort=None, pane='worker', surface='tab',
                        generation=1, timeout=timeout, stall_timeout=180, transcript=str(path))
        clock = [0]
        def native(path, manifest):
            if progress:
                with path.open('a') as stream:
                    stream.write('native progress\n')
            return True, 'exact', json.dumps(dict(task_status='completed', report='verified')) if clock[0] >= complete_at else None, None
        def advance(seconds):
            clock[0] += 100
        def pane(manifest):
            return dict(agent=dict(status='blocked' if waiting and waiting[0] <= clock[0] < waiting[1] else 'working'))
        process = type('Process', (), dict(pid=123, poll=lambda self: None))()
        with patch.object(team, 'owned_processes', return_value={}), \
                patch.object(team, 'transcript_report', side_effect=native), \
                patch.object(team, 'managed_pane', side_effect=pane), \
                patch.object(team.time, 'monotonic', side_effect=lambda: clock[0]), \
                patch.object(team.time, 'sleep', side_effect=advance):
            return team.monitor_managed(self.job, attempt, manifest, process, {}, False)

    def test_progressing_work_completes_beyond_old_300_second_limit(self):
        self.assertEqual(self.monitor_clock()['state'], 'completed')

    def test_explicit_budget_is_not_stall_or_retry(self):
        state = self.monitor_clock(timeout=300)
        self.assertEqual((state['state'], state['blocker_kind']), ('timed_out', 'time_budget'))
        self.assertNotIn(state['blocker_kind'], team.RECOVERABLE)

    def test_no_native_progress_still_triggers_recovery(self):
        state = self.monitor_clock(progress=False)
        self.assertEqual(state['blocker_kind'], 'stalled')
        self.assertIn(state['blocker_kind'], team.RECOVERABLE)

    def test_input_wait_pauses_time_budget_and_stall_deadline(self):
        self.assertEqual(self.monitor_clock(timeout=200, waiting=(0, 300))['state'], 'completed')

    def test_explicit_budget_wins_when_both_deadlines_expire(self):
        state = self.monitor_clock(timeout=180, progress=False)
        self.assertEqual(state['blocker_kind'], 'time_budget')
        self.assertNotIn(state['blocker_kind'], team.RECOVERABLE)

    def test_input_wait_preserves_time_already_spent(self):
        state = self.monitor_clock(timeout=250, waiting=(200, 300), complete_at=500)
        self.assertEqual((state['state'], state['blocker_kind']), ('timed_out', 'time_budget'))

    def test_native_auth_codes_reach_monitor_without_becoming_recoverable(self):
        path = self.job / 'native.jsonl'
        attempt = self.job / 'attempt-1'
        attempt.mkdir()
        process = type('Process', (), dict(pid=123, poll=lambda self: None))()
        for agent in ('codex', 'claude'):
            for code in ('permission_denied', 'authentication_error', 'invalid_api_key'):
                with self.subTest(agent=agent, code=code):
                    message = 'Usage limit reached; model_not_found'
                    if agent == 'codex':
                        user = dict(type='response_item', payload=dict(role='user', content='FLOWMUX_TEAM_JOB:mine'))
                        final = dict(type='event_msg', payload=dict(type='error', codex_error_info=code, message=message))
                    else:
                        user = dict(type='user', sessionId='exact', message=dict(content='FLOWMUX_TEAM_JOB:mine'))
                        final = dict(type='assistant', isApiErrorMessage=True, error=code,
                                     message=dict(content=[dict(type='text', text=message)]))
                    path.write_text(json.dumps(user) + '\n' + json.dumps(final))
                    team.write_json(self.job / 'status.json', dict(state='pending'))
                    manifest = dict(agent=agent, model='chosen', effort=None, pane='worker', surface='tab',
                                    token='mine', generation=1, timeout=0, stall_timeout=180, transcript=str(path))
                    with patch.object(team, 'owned_processes', return_value={}):
                        state = team.monitor_managed(self.job, attempt, manifest, process, {}, False)
                    self.assertEqual(state['blocker_kind'], 'input_required')
                    self.assertNotIn(state['blocker_kind'], team.RECOVERABLE)
                    self.assertEqual((attempt / 'result.md').read_text(), message)

    def test_streamed_auth_errors_take_priority_over_quota_and_unavailable_text(self):
        for error in [dict(code='permission_denied', message='quota model_not_found'),
                      dict(error=dict(code='invalid_api_key'), message='unsupported_client'),
                      dict(status='UNAUTHENTICATED', response='Usage limit reached')]:
            self.assertEqual(team.native_failure(error), 'input_required')
        self.assertEqual(team.native_failure(dict(code='model_not_found')), 'unavailable')
        self.assertEqual(team.native_failure(dict(code='resource_exhausted')), 'quota')
        self.assertEqual(team.native_failure(dict(code='usage_limit_exceeded', message='Contact authentication support')), 'quota')
        self.assertEqual(team.native_failure(dict(code='model_not_found', message='Check account permissions')), 'unavailable')

    def test_model_aliases_and_available_alternatives(self):
        for alias, (provider, model) in team.MODEL_ALIASES.items():
            args = type('Args', (), dict(agent='auto', complexity='complex', model=alias, effort='high'))()
            with patch.object(team.shutil, 'which', side_effect=lambda agent: '/bin/' + agent if agent in ('codex', 'claude') else None):
                selection = team.model_route(args)
                self.assertEqual((selection['agent'], selection['model']), (provider, model))
                routes = team.route_plan(args)
                self.assertEqual(routes[0], selection)
                self.assertEqual(len(routes), len({(r['agent'], r['model']) for r in routes}))
                self.assertFalse(any(r['agent'] in ('gemini', 'agy') for r in routes))
                args.no_fallback = True
                self.assertEqual(team.route_plan(args), [selection])

    def test_quota_skips_other_models_on_same_account_and_attempts_are_bounded(self):
        routes = [dict(agent='codex', model='sol'), dict(agent='codex', model='terra'),
                  dict(agent='agy', model='opus')]
        manifest = dict(routes=routes, max_attempts=2, executables={'codex': '/codex', 'agy': '/agy'})
        attempt = dict(routes[0], blocker_kind='quota')
        self.assertEqual(team.next_route(manifest, [attempt]), routes[2])
        self.assertIsNone(team.next_route(manifest, [attempt, dict(routes[2], blocker_kind='quota')]))
        attempt['blocker_kind'] = 'unavailable'
        self.assertEqual(team.next_route(manifest, [attempt]), routes[1])

    def test_google_providers_require_native_final_results(self):
        path = self.job / 'stdout.jsonl'
        answer = json.dumps(dict(task_status='completed', report='verified'))
        for agent, events, final in [
            ('agy', [{'event': 'init', 'conversation_id': 'exact'},
                     {'event': 'step_update', 'step_update': {'state': 'DONE', 'text_delta': answer}}],
             {'event': 'result', 'result': {'status': 'SUCCESS', 'conversation_id': 'exact', 'response': answer}}),
            ('gemini', [{'type': 'init', 'session_id': 'exact'},
                        {'type': 'message', 'role': 'assistant', 'content': answer}],
             {'type': 'result', 'status': 'success'})]:
            path.write_text(''.join(json.dumps(e) + '\n' for e in events))
            self.assertFalse(team.native_stream(path, agent)[3])
            with path.open('a') as stream:
                stream.write(json.dumps(final) + '\n')
            self.assertEqual(team.native_stream(path, agent), ('exact', answer, None, True))

    def test_google_yolo_and_exact_conversation_resume(self):
        (self.job / 'prompt.txt').write_text('literal prompt')
        for agent in ('gemini', 'agy'):
            manifest = dict(agent=agent, executable=agent, token='new', session_id='exact',
                            resume_session=True, model='chosen', effort=None)
            argv = team.agent_command(self.job, manifest)
            flag = '--resume' if agent == 'gemini' else '--conversation'
            self.assertEqual(argv[argv.index(flag) + 1], 'exact')
            self.assertIn('stream-json', argv)
            self.assertEqual(argv[-1], 'literal prompt')
            self.assertIn('yolo' if agent == 'gemini' else '--dangerously-skip-permissions', argv)
            self.assertNotIn('--skip-trust', argv)

    def test_nontext_native_report_retains_evidence_and_allows_recovery(self):
        attempt = self.job / 'attempt-1'
        attempt.mkdir()
        team.write_json(self.job / 'status.json', dict(state='pending'))
        response = dict(task_status='completed', report='wrong native type')
        event = dict(event='result', result=dict(status='SUCCESS', conversation_id='exact', response=response))
        (attempt / 'stdout.jsonl').write_text(json.dumps(event) + '\n')
        (attempt / 'stderr.txt').write_text('')
        manifest = dict(agent='agy', model='chosen', effort=None, pane='worker', surface='surface',
                        generation=1, timeout=5, stall_timeout=5)
        process = type('Process', (), dict(pid=123, returncode=0, poll=lambda self: 0))()
        with patch.object(team, 'owned_processes', return_value={}), contextlib.redirect_stdout(io.StringIO()):
            state = team.monitor_managed(self.job, attempt, manifest, process, {}, True)
        self.assertEqual(state['state'], 'failed')
        self.assertEqual(state['blocker_kind'], 'invalid_report')
        self.assertIn(state['blocker_kind'], team.RECOVERABLE)
        self.assertEqual(json.loads((attempt / 'answer.txt').read_text()), response)

    def test_gemini_final_report_excludes_commentary_tools_and_rejected_answers(self):
        path = self.job / 'stdout.jsonl'
        answer = json.dumps(dict(task_status='completed', report='accepted final answer'))
        events = [dict(type='init', session_id='exact'),
                  dict(type='message', role='assistant', content='I will inspect the file.'),
                  dict(type='tool_use', tool_id='read'),
                  dict(type='tool_result', tool_id='read', output='Do not accept tool output'),
                  dict(type='message', role='assistant', content='rejected answer'),
                  dict(type='error', severity='warning', message='Agent execution blocked: verify the result'),
                  dict(type='message', role='assistant', content=answer[:20], delta=True),
                  dict(type='message', role='assistant', content=answer[20:], delta=True),
                  dict(type='result', status='success')]
        path.write_text(''.join(json.dumps(e) + '\n' for e in events))
        self.assertEqual(team.native_stream(path, 'gemini'), ('exact', answer, None, True))

    def test_cleanup_refuses_lead_changed_sessions_and_extra_tabs(self):
        manifest = dict(cli='flowmux', socket='socket', workspace='team', source_pane='lead',
                        pane='worker', surface='surface', agent='codex', session_id='exact')
        tab = dict(id='surface', kind='terminal', agent=dict(name='codex', session_id='exact'))
        for change in ('none', 'lead', 'extra-tab', 'session', 'workspace', 'source'):
            current = dict(manifest)
            worker = dict(id='worker', tabs=[json.loads(json.dumps(tab))])
            workspace = dict(id='team', location=dict(type='team'), panes=[dict(id='lead', tabs=[]), worker])
            if change == 'lead':
                current['pane'] = 'lead'
            elif change == 'extra-tab':
                worker['tabs'].append(dict(id='editor', kind='editor'))
            elif change == 'session':
                worker['tabs'][0]['agent']['session_id'] = 'someone-else'
            elif change == 'workspace':
                workspace['location']['type'] = 'local'
            elif change == 'source':
                workspace['panes'].pop(0)
            with patch.object(team, 'flowmux', return_value=dict(tree=dict(workspaces=[workspace]))) as rpc:
                if change == 'none':
                    self.assertEqual(team.managed_pane(current), tab)
                else:
                    with self.assertRaises(ValueError):
                        team.managed_pane(current)
                self.assertEqual(rpc.call_args.args[-1], 'tree')

    def test_uncertain_replacement_is_not_replayed(self):
        manifest = dict(generation=1, cli='flowmux', socket='socket', workspace='team', source_pane='lead',
                        pane='worker', surface='surface', agent='codex', model='sol', effort='high',
                        executables={'claude': '/claude'}, cwd=str(self.job), role='test')
        state = dict(state='recovering', failed_state='failed')
        team.write_json(self.job / 'job.json', manifest)
        team.write_json(self.job / 'status.json', state)
        team.write_json(self.job / 'recovery.json', dict(generation=1, assignment=str(self.job), state=state,
                        next=dict(agent='claude', model='opus', effort='high')))
        calls = []
        def rpc(*args):
            calls.append(args[2])
            if args[2] == 'team-spawn':
                raise subprocess.TimeoutExpired('team-spawn', 15)
            return dict(tree=dict(workspaces=[]))
        with patch.object(team, 'managed_pane'), patch.object(team, 'flowmux', side_effect=rpc), \
                patch.object(team, 'write_launcher', return_value=self.job / 'launch.sh'):
            self.assertEqual(team.recover(self.job, 1), 1)
            self.assertEqual(team.recover(self.job, 1), 1)
        self.assertEqual(calls, ['close-tab', 'tree', 'team-spawn'])
        self.assertEqual(json.loads((self.job / 'status.json').read_text())['blocker_kind'], 'recovery_failed')


class UsageRoutingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix='fm-routing-test-')
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        environment = patch.dict(os.environ, XDG_STATE_HOME=str(self.root), CODEX_HOME=str(self.root / 'codex'),
                                 CLAUDE_CONFIG_DIR=str(self.root / 'claude'))
        environment.start()
        self.addCleanup(environment.stop)
        self.clock = patch.object(team.time, 'time', return_value=10000)
        self.now = self.clock.start()
        self.addCleanup(self.clock.stop)
        installed = patch.object(team.shutil, 'which', side_effect=lambda agent: '/bin/' + agent if agent in ('codex', 'claude') else None)
        installed.start()
        self.addCleanup(installed.stop)
        self.route = dict(agent='codex', model='gpt-6.1-sol')
        self.args = type('Args', (), dict(agent='auto', complexity='standard', model=None, effort=None, no_fallback=False))()

    def snapshot(self, agent='codex', windows=None, **fields):
        profile, key = team.usage_identity(agent)
        directory = team.state_directory() / 'team-usage'
        directory.mkdir(parents=True, exist_ok=True)
        value = dict(schema=1, agent=agent, profile=profile, collected_at=10000,
                     windows=windows if windows is not None else [dict(scope=None, used_percent=100, resets_at=11000)])
        value.update(fields)
        path = directory / (key + '.json')
        team.write_json(path, value)
        return path

    def test_known_exhausted_provider_is_skipped_before_launch(self):
        self.snapshot()
        routes, observed = team.select_routes(self.args)
        self.assertEqual(routes[0]['agent'], 'claude')
        self.assertFalse(any(route['agent'] == 'codex' for route in routes))
        self.assertEqual(observed[0]['status'], 'exhausted')
        self.args.no_fallback = True
        with self.assertRaisesRegex(ValueError, 'no pane created'):
            team.select_routes(self.args)

    def test_stale_unknown_wrong_profile_and_invalid_observations_do_not_block(self):
        self.assertEqual(team.usage_status(self.route)['status'], 'unknown')
        for fields, expected in [(dict(collected_at=9600), 'stale'), (dict(profile='/another-account'), 'unknown'),
                                 (dict(error='offline'), 'unknown'), (dict(collected_at=11000), 'unknown'),
                                 (dict(windows='malformed'), 'unknown'), (dict(schema=2), 'unknown')]:
            self.snapshot(**fields)
            self.assertEqual(team.usage_status(self.route)['status'], expected)
            self.assertEqual(team.select_routes(self.args)[0][0]['agent'], 'codex')
        self.snapshot().write_text('{truncated')
        self.assertEqual(team.usage_status(self.route)['status'], 'unknown')

    def test_scoped_limits_only_block_the_relevant_claude_model(self):
        self.snapshot('claude', [dict(scope='Sonnet', used_percent=100, resets_at=11000),
                                 dict(scope=None, used_percent=40, resets_at=12000)])
        self.assertEqual(team.usage_status(dict(agent='claude', model='sonnet'))['status'], 'exhausted')
        self.assertEqual(team.usage_status(dict(agent='claude', model='opus'))['status'], 'available')
        self.snapshot('claude', [dict(scope=None, used_percent=100, resets_at=11000)])
        self.assertEqual(team.usage_status(dict(agent='claude', model='opus'))['status'], 'exhausted')

    def test_expired_or_nonfinite_windows_are_not_current_exhaustion(self):
        for window in [dict(scope=None, used_percent=100, resets_at=9999),
                       dict(scope=None, used_percent=float('nan'), resets_at=11000),
                       dict(scope=None, used_percent=True, resets_at=11000),
                       dict(scope='Unknown model family', used_percent=100, resets_at=11000)]:
            self.snapshot(windows=[window])
            self.assertEqual(team.usage_status(self.route)['status'], 'unknown')

    def test_claude_web_limits_do_not_exclude_cli_workers(self):
        for scope, opus, sonnet in [('Opus · Claude.ai', 'unknown', 'unknown'),
                                    ('Opus · Claude Code', 'exhausted', 'unknown'),
                                    ('Opus · claude_code', 'exhausted', 'unknown'),
                                    ('Claude Code', 'exhausted', 'exhausted'),
                                    ('claude_code', 'exhausted', 'exhausted')]:
            with self.subTest(scope=scope):
                self.snapshot('claude', [dict(scope=scope, used_percent=100, resets_at=11000)])
                self.assertEqual(team.usage_status(dict(agent='claude', model='opus'))['status'], opus)
                self.assertEqual(team.usage_status(dict(agent='claude', model='sonnet'))['status'], sonnet)

    def test_native_quota_is_shared_until_known_reset_even_after_usage_becomes_stale(self):
        self.snapshot()
        team.remember_quota(self.route)
        self.now.return_value = 10400
        self.assertEqual(team.usage_status(self.route)['until'], 11000)
        self.assertEqual(team.select_routes(self.args)[0][0]['agent'], 'claude')
        self.now.return_value = 11001
        self.assertEqual(team.select_routes(self.args)[0][0]['agent'], 'codex')

    def test_unknown_quota_uses_bounded_backoff_and_does_not_cross_profiles(self):
        team.remember_quota(self.route)
        self.assertEqual(team.usage_status(self.route)['status'], 'exhausted')
        self.assertEqual(team.usage_status(self.route, dict(CODEX_HOME=str(self.root / 'other')))['status'], 'unknown')
        profile, key = team.usage_identity('codex')
        saved = team.state_directory() / 'team-quota' / (key + '.json')
        self.assertEqual(saved.stat().st_mode & 0o777, 0o600)
        self.now.return_value = 10301
        self.assertEqual(team.usage_status(self.route)['status'], 'unknown')

    def test_only_newer_available_usage_releases_a_native_quota_hold(self):
        team.remember_quota(self.route)
        self.snapshot(windows=[dict(scope=None, used_percent=25, resets_at=11000)], collected_at=9999)
        self.assertEqual(team.usage_status(self.route)['status'], 'exhausted')
        self.now.return_value = 10002
        self.snapshot(windows=[dict(scope=None, used_percent=25, resets_at=11000)], collected_at=10001)
        self.assertEqual(team.usage_status(self.route)['status'], 'available')

    def test_concurrent_native_quota_records_remain_complete(self):
        from concurrent.futures import ThreadPoolExecutor
        with ThreadPoolExecutor(max_workers=4) as workers:
            list(workers.map(lambda _: team.remember_quota(self.route), range(12)))
        self.assertEqual(team.usage_status(self.route)['status'], 'exhausted')
        directory = team.state_directory() / 'team-quota'
        self.assertEqual(len(list(directory.glob('*.json'))), 1)
        self.assertFalse(list(directory.glob('.*')))

    def test_new_quota_does_not_restore_a_released_hold(self):
        self.snapshot()
        team.remember_quota(self.route)
        self.now.return_value = 10002
        self.snapshot(windows=[dict(scope=None, used_percent=25, resets_at=11000)], collected_at=10001)
        self.assertEqual(team.usage_status(self.route)['status'], 'available')
        team.remember_quota(self.route)
        self.assertEqual(team.usage_status(self.route)['until'], 10302)

    def test_fresh_usage_keeps_a_later_reset_than_the_native_backoff(self):
        team.remember_quota(self.route)
        self.assertEqual(team.usage_status(self.route)['until'], 10300)
        self.now.return_value = 10002
        self.snapshot(collected_at=10001)
        self.assertEqual(team.usage_status(self.route)['until'], 11000)
        team.remember_quota(self.route)
        self.now.return_value = 10400
        self.assertEqual(team.usage_status(self.route)['until'], 11000)

    def test_released_hold_stays_released_after_usage_expires_or_refresh_fails(self):
        self.snapshot()
        team.remember_quota(self.route)
        self.now.return_value = 10002
        self.snapshot(windows=[dict(scope=None, used_percent=25, resets_at=11000)], collected_at=10001)
        self.assertEqual(team.usage_status(self.route)['status'], 'available')
        self.now.return_value = 10400
        self.assertEqual(team.usage_status(self.route)['status'], 'stale')
        self.snapshot(collected_at=10400, error='offline')
        self.assertEqual(team.usage_status(self.route)['status'], 'unknown')
        team.remember_quota(self.route)
        self.assertEqual(team.usage_status(self.route)['until'], 10700)

    def test_releasing_an_old_hold_cannot_erase_a_concurrent_new_quota(self):
        self.snapshot()
        team.remember_quota(self.route)
        self.now.return_value = 10002
        self.snapshot(windows=[dict(scope=None, used_percent=25, resets_at=11000)], collected_at=10001)
        read = team.observation
        injected = False
        def concurrent_quota(path, agent, profile):
            nonlocal injected
            value = read(path, agent, profile)
            if path.parent.name == 'team-quota' and not injected:
                injected = True
                team.remember_quota(self.route)
            return value
        with patch.object(team, 'observation', side_effect=concurrent_quota):
            self.assertEqual(team.usage_status(self.route)['status'], 'exhausted')
        self.assertEqual(team.usage_status(self.route)['until'], 10302)


if __name__ == '__main__':
    unittest.main()
