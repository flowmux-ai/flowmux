#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise team launch, result validation and timeout cleanup without an API account."""
import contextlib
import importlib.util
import io
import json
import os
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
        self.temporary = tempfile.TemporaryDirectory(prefix="fm-team-test-")
        self.addCleanup(self.temporary.cleanup)
        self.job = Path(self.temporary.name)
        self.socket = self.job / 'window.sock'
        self.socket.touch()
        self.env = dict(os.environ, FLOWMUX_PANE_ID='child', FLOWMUX_SOCKET_PATH=str(self.socket))

    def fixture(self, code, agent='codex', timeout=5):
        executable = self.job / agent
        executable.write_text(f'#!{sys.executable}\n' + code)
        executable.chmod(0o700)
        (self.job / 'prompt.txt').write_text('literal task: $(touch NEVER) `false` \\n 한글')
        team.write_json(self.job / 'job.json', {
            'agent': agent, 'executable': str(executable), 'cwd': str(self.job),
            'role': 'reviewer', 'allow_edits': False, 'timeout': timeout,
            'pane': 'child', 'socket': str(self.socket),
        })
        team.write_json(self.job / 'status.json', {'state': 'pending'})

    def run_worker(self):
        return subprocess.run([sys.executable, str(HELPER), '_worker', str(self.job)],
                              env=self.env, capture_output=True, text=True, timeout=10)

    def state(self):
        return json.loads((self.job / 'status.json').read_text())['state']

    def test_success_and_reentry_do_not_repeat_work(self):
        self.fixture("import json,pathlib,sys\n"
                     "p=pathlib.Path(sys.argv[sys.argv.index('--output-last-message')+1])\n"
                     "p.write_text(json.dumps({'task_status':'completed','report':sys.stdin.read()}))\n")
        result = self.run_worker()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.state(), 'completed')
        self.assertEqual((self.job / 'result.md').read_text(), (self.job / 'prompt.txt').read_text())
        self.assertFalse((self.job / 'NEVER').exists())
        self.assertEqual(self.run_worker().returncode, 1)
        self.assertEqual(self.state(), 'completed')

    def test_error_exit_and_missing_answer_are_not_success(self):
        for code in ('raise SystemExit(7)', 'pass'):
            with self.subTest(code=code):
                self.fixture(code)
                (self.job / 'started').unlink(missing_ok=True)
                self.assertEqual(self.run_worker().returncode, 1)
                self.assertEqual(self.state(), 'failed')
                self.assertFalse((self.job / 'result.md').exists())

    def test_claude_error_and_denied_tools_are_not_success(self):
        for response in ({'is_error': True, 'result': 'quota'},
                         {'result': 'done', 'permission_denials': [{'tool_name': 'Read'}]},
                         {'result': ''}, {'result': 'verified'}):
            with self.subTest(response=response):
                self.fixture('print(' + repr(json.dumps(response)) + ')', agent='claude')
                (self.job / 'started').unlink(missing_ok=True)
                result = self.run_worker()
                self.assertEqual(result.returncode, 1)

    def test_task_outcomes_for_both_providers(self):
        for agent in ('codex', 'claude'):
            for outcome in ('completed', 'blocked', 'failed'):
                with self.subTest(agent=agent, outcome=outcome):
                    answer = json.dumps({'task_status': outcome, 'report': 'Evidence or blocker'})
                    code = ('import pathlib,sys\n'
                            "pathlib.Path(sys.argv[sys.argv.index('--output-last-message')+1])"
                            f'.write_text({answer!r})') if agent == 'codex' else (
                            'print(' + repr(json.dumps({'result': answer})) + ')')
                    self.fixture(code, agent=agent)
                    (self.job / 'started').unlink(missing_ok=True)
                    expected = 0 if outcome == 'completed' else 1
                    self.assertEqual(self.run_worker().returncode, expected)
                    self.assertEqual(self.state(), outcome)
                    output = io.StringIO()
                    with contextlib.redirect_stdout(output):
                        self.assertEqual(team.inspect(self.job, .01), expected)
                    report = json.loads(output.getvalue())
                    self.assertEqual(report['result'], 'Evidence or blocker')
                    self.assertEqual(report['exit_code'], 0)

    def test_malformed_reports_fail_closed(self):
        for answer in ('BLOCKED: no work done', '{}', '[]', 'null',
                       '{"task_status":"unknown","report":"done"}',
                       '{"task_status":"completed","report":42}',
                       '{"task_status":"completed","report":" "}'):
            with self.subTest(answer=answer):
                with self.assertRaises(ValueError):
                    team.task_report(answer)

    def test_worker_normalizes_socket_alias_and_rejects_other_socket(self):
        alias = self.job / 'selected.sock'
        alias.symlink_to(self.socket)
        self.env['FLOWMUX_SOCKET_PATH'] = str(alias)
        answer = json.dumps({'task_status': 'completed', 'report': 'verified'})
        self.fixture('print(' + repr(json.dumps({'result': answer})) + ')', agent='claude')
        self.assertEqual(self.run_worker().returncode, 0)
        other = self.job / 'other.sock'
        other.touch()
        alias.unlink()
        alias.symlink_to(other)
        (self.job / 'started').unlink()
        self.fixture("from pathlib import Path\nPath('UNEXPECTED').touch()")
        self.assertEqual(self.run_worker().returncode, 1)
        self.assertFalse((self.job / 'UNEXPECTED').exists())

    def test_wrong_pane_refuses_to_launch_agent(self):
        self.fixture("from pathlib import Path\nPath('UNEXPECTED').touch()")
        self.env['FLOWMUX_PANE_ID'] = 'source'
        self.assertEqual(self.run_worker().returncode, 1)
        self.assertFalse((self.job / 'UNEXPECTED').exists())
        self.assertEqual(self.state(), 'failed')

    def test_deadline_kills_agent_and_its_tool(self):
        self.fixture("import pathlib,subprocess,sys,time\n"
                     "subprocess.Popen([sys.executable, '-c', "
                     "\"import pathlib,time; time.sleep(2); pathlib.Path('LEAK').touch()\"])\n"
                     "time.sleep(20)\n", timeout=1)
        self.assertEqual(self.run_worker().returncode, 1)
        self.assertEqual(self.state(), 'timed_out')
        time.sleep(1.3)
        self.assertFalse((self.job / 'LEAK').exists())

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
                return {'tree': {'workspaces': [{'id': 'ws', 'location': {'type': 'local'},
                                                 'panes': [{'id': 'source'}]}]}}
            if args[0] == 'split':
                return {'pane_split_done': {'new_pane': 'child'}}
            if args[0] == 'new-tab':
                launcher = Path(args[-1])
                self.assertTrue(os.access(launcher, os.X_OK))
                self.assertIn('_worker', launcher.read_text())
                return {'surface_created': {'pane': 'child', 'id': 'tab'}}
            return 'ok'
        args = type('Args', (), dict(socket=str(alias), pane='source', cwd=self.job,
                    task_file=self.job / 'prompt.txt', role='reviewer', agent='codex',
                    cli='flowmux', allow_edits=False, timeout=5))()
        artifacts = self.job / "job 'quoted' $path"
        artifacts.mkdir()
        with patch.object(team, 'flowmux', side_effect=cli), \
                patch.object(team.shutil, 'which', return_value='/bin/true'), \
                patch.object(team.tempfile, 'mkdtemp', return_value=str(artifacts)), \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(team.start(args), 0)
        self.assertEqual([c[0] for c in calls], ['tree', 'split', 'focus-pane', 'new-tab'])
        self.assertEqual(calls[2], ('focus-pane', 'child'))
        self.assertEqual(json.loads((artifacts / 'job.json').read_text())['surface'], 'tab')
        self.assertEqual(json.loads((artifacts / 'job.json').read_text())['socket'], str(self.socket))

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
                        return subprocess.CompletedProcess(args, 0, json.dumps({'pane': 'child', 'job': '/tmp/job'}), '')
                    return subprocess.CompletedProcess(args, 0, json.dumps(report), '')
                with patch.dict(os.environ, TEAM_SKILL=str(HELPER.parents[1]), TEAM_AGENT='codex'), \
                        patch.object(subprocess, 'run', side_effect=run), \
                        patch.object(tempfile, 'mkdtemp', return_value=str(self.job)), \
                        contextlib.redirect_stdout(io.StringIO()):
                    if report and report.get('task_status') == 'completed':
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
                manifest = dict(agent=agent, executable=agent, allow_edits=edits)
                args = team.agent_command(self.job, manifest)
                self.assertFalse(any('danger' in arg or 'bypass' in arg for arg in args))
                if agent == 'codex':
                    self.assertIn('workspace-write' if edits else 'read-only', args)
                else:
                    self.assertEqual(args[args.index('--permission-mode') + 1], 'dontAsk')
                    self.assertNotIn('Bash', args[args.index('--tools') + 1])

    def test_cli_error_preserves_diagnostic(self):
        result = subprocess.CompletedProcess([], 1, '', 'pane not found')
        with patch.object(team.subprocess, 'run', return_value=result):
            with self.assertRaisesRegex(RuntimeError, 'pane not found'):
                team.flowmux('flowmux', '/test.sock', 'split', 'missing')


if __name__ == '__main__':
    unittest.main()
