#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check gate failure propagation and isolation without launching a desktop."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
STUB = r'''
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps({'name': name, 'args': args,
        'socket': os.environ.get('FLOWMUX_SOCKET_PATH'),
        'tmp': os.environ.get('TMPDIR'),
        'runtime': os.environ.get('FLOWMUX_RUNTIME_DIR'),
        'skills': os.environ.get('FLOWMUX_SKILLS_SMOKE_ONLY'),
        'agents': os.environ.get('FLOWMUX_AGENT_SMOKE_ONLY')}) + '\n')
if name == 'uname': print(os.environ.get('TEST_OS', 'Linux'))
if name == 'xvfb-run': sys.exit(int(os.environ.get('TEST_EXIT', '0')))
if name == 'cargo' and args[:2] == ['llvm-cov', 'show-env']:
    sys.exit(23)
if name == 'python3' and args == ['scripts/test-agent-unit.py']:
    sys.exit(int(os.environ.get('AGENT_UNIT_EXIT', '0')))
if name == 'cargo' and 'macos_native' in args:
    if os.environ.get('FLOWMUX_AGENT_SMOKE_ONLY'):
        print('MACOS_NATIVE_AGENT_HOOKS_OK')
        sys.exit(int(os.environ.get('AGENT_GUI_EXIT', '0')))
    if os.environ.get('FLOWMUX_SKILLS_SMOKE_ONLY'):
        print('MACOS_NATIVE_SKILLS_INSTALL_UPDATE_OK')
        sys.exit(int(os.environ.get('SKILLS_TEST_EXIT', '0')))
    print('MACOS_NATIVE_SMOKE_OK')
    sys.exit(int(os.environ.get('TEST_EXIT', '0')))
'''


class RunnerTests(unittest.TestCase):
    def run_script(self, script, *args, **extra):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ['uname', 'rustc', 'rustup', 'cargo', 'pkg-config', 'xvfb-run', 'python3']:
                stub = root / name
                stub.write_text(f'#!{sys.executable}\n' + STUB)
                stub.chmod(0o755)
            env = dict(os.environ, PATH=f'{root}:{os.environ["PATH"]}',
                       CALLS=str(root / 'calls'), CARGO_TARGET_DIR=str(root / 'target'),
                       FLOWMUX_SOCKET_PATH='/do/not/use/live.sock', **extra)
            result = subprocess.run(['bash', str(ROOT / 'scripts' / script), *args],
                                    env=env, capture_output=True, text=True)
            calls = [json.loads(line) for line in (root / 'calls').read_text().splitlines()]
            return result, calls

    def test_linux_failure_keeps_reports_and_failure_status(self):
        result, calls = self.run_script('test-ci.sh', 'linux', TEST_EXIT='17')
        self.assertEqual(result.returncode, 17, result.stdout + result.stderr)
        desktop = [call for call in calls if call['name'] == 'xvfb-run']
        self.assertEqual(len(desktop), 1, 'a failed test must not be retried')
        self.assertIsNone(desktop[0]['socket'])
        self.assertTrue(desktop[0]['runtime'].startswith('/tmp/fm-ci.'))
        self.assertFalse(Path(desktop[0]['tmp']).exists(), 'temporary state must be removed')
        reports = [c for c in calls if c['args'][:2] == ['llvm-cov', 'report']]
        self.assertEqual(len(reports), 3)
        self.assertIn('--fail-under-lines', reports[-1]['args'])

    def test_macos_pipeline_does_not_hide_native_failure(self):
        result, calls = self.run_script('test-ci.sh', 'macos', TEST_OS='Darwin', TEST_EXIT='19')
        self.assertEqual(result.returncode, 19, result.stdout + result.stderr)
        self.assertEqual(sum('macos_native' in c['args'] for c in calls), 1)

    def test_macos_skill_gate_runs_and_propagates_failure(self):
        for exit_code in (0, 21):
            with self.subTest(exit_code=exit_code):
                result, calls = self.run_script('test-ci.sh', 'macos', TEST_OS='Darwin',
                                                SKILLS_TEST_EXIT=str(exit_code))
                self.assertEqual(result.returncode, exit_code, result.stdout + result.stderr)
                native = [c for c in calls if 'macos_native' in c['args']]
                self.assertEqual(len(native), 3 if exit_code == 0 else 2)
                self.assertIsNone(native[0]['skills'])
                self.assertEqual(native[1]['skills'], '1')

    def test_macos_agent_gates_run_and_propagate_failure(self):
        for variable in ('AGENT_UNIT_EXIT', 'AGENT_GUI_EXIT'):
            with self.subTest(variable=variable):
                result, calls = self.run_script('test-ci.sh', 'macos', TEST_OS='Darwin',
                                                **{variable: '27'})
                self.assertEqual(result.returncode, 27, result.stdout + result.stderr)
                units = [c for c in calls if c['args'] == ['scripts/test-agent-unit.py']]
                self.assertEqual(len(units), 1)
                self.assertIsNone(units[0]['socket'])
                hooks = [c for c in calls if c['agents'] == '1']
                self.assertEqual(len(hooks), 0 if variable == 'AGENT_UNIT_EXIT' else 1)

    @unittest.skipUnless(sys.platform == "darwin", "agent unit worker is used by the macOS gate")
    def test_agent_unit_worker_cleans_up_descendants(self):
        # Exercise the real fork/timeout/signal paths, not the gate's exit-code stub.
        driver = """
import importlib.util, sys
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("runner", sys.argv[1])
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
real_run = runner.subprocess.run
def short_run(*args, **kwargs):
    kwargs['timeout'] = .5 if sys.argv[2] == 'timeout' else 10
    return real_run(*args, **kwargs)
runner.subprocess.run = short_run
raise SystemExit(runner.main())
"""
        job = """
import json, os, pathlib, subprocess, sys, time
if os.environ['WORKER_CASE'] == 'empty':
    pathlib.Path(os.environ['CHILD_INFO']).write_text(json.dumps([os.getpid(), os.getpgrp()]))
    sys.exit(0)
child = subprocess.Popen([sys.executable, '-c',
    'import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)'],
    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path(os.environ['CHILD_INFO']).write_text(json.dumps([child.pid, os.getpgrp()]))
if os.environ['WORKER_CASE'] in ('success', 'failure'):
    sys.exit(0 if os.environ['WORKER_CASE'] == 'success' else 27)
time.sleep(60)
"""
        for mode, expected in [('empty', 0), ('success', 0), ('failure', 27), ('timeout', 1),
                               ('terminate', 143), ('interrupt', 130), ('kill', -9)]:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                cargo = root / 'cargo'
                cargo.write_text(f'#!{sys.executable}\n' + job)
                cargo.chmod(0o755)
                info = root / 'child.json'
                env = dict(os.environ, PATH=f'{root}:{os.environ["PATH"]}',
                           CHILD_INFO=str(info), WORKER_CASE=mode)
                with (root / 'output').open('w') as output:
                    process = subprocess.Popen([sys.executable, '-c', driver,
                        str(ROOT / 'scripts/test-agent-unit.py'), mode],
                        env=env, stdout=output, stderr=subprocess.STDOUT)
                    try:
                        deadline = time.monotonic() + 10
                        while not info.exists() or not info.read_text():
                            self.assertIsNone(process.poll(), (root / 'output').read_text())
                            self.assertLess(time.monotonic(), deadline)
                            time.sleep(.02)
                        child, group = json.loads(info.read_text())
                        self.assertNotEqual(group, os.getpgrp())
                        termination = {'terminate': signal.SIGTERM, 'interrupt': signal.SIGINT,
                                       'kill': signal.SIGKILL}.get(mode)
                        if termination is not None:
                            process.send_signal(termination)
                        self.assertEqual(process.wait(timeout=10), expected,
                                         (root / 'output').read_text())
                        deadline = time.monotonic() + 2
                        while True:
                            try:
                                os.kill(child, 0)
                            except ProcessLookupError:
                                break
                            self.assertLess(time.monotonic(), deadline, 'test descendant survived')
                            time.sleep(.02)
                    finally:
                        # Clean only this fixture's detached group, including on red tests.
                        if info.exists() and info.read_text():
                            _, group = json.loads(info.read_text())
                            if group != os.getpgrp():
                                try:
                                    os.killpg(group, signal.SIGKILL)
                                except ProcessLookupError:
                                    pass
                        if process.poll() is None:
                            process.kill()
                        process.wait(timeout=10)

    def test_coverage_environment_failure_cannot_run_uninstrumented_tests(self):
        result, calls = self.run_script('test-coverage.sh')
        self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
        self.assertFalse(any(c['name'] == 'cargo' and c['args'][0] == 'test' for c in calls))


if __name__ == '__main__':
    unittest.main()
