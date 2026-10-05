#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check gate failure propagation and isolation without launching a desktop."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
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
        'skills': os.environ.get('FLOWMUX_SKILLS_SMOKE_ONLY')}) + '\n')
if name == 'uname': print(os.environ.get('TEST_OS', 'Linux'))
if name == 'xvfb-run': sys.exit(int(os.environ.get('TEST_EXIT', '0')))
if name == 'cargo' and args[:2] == ['llvm-cov', 'show-env']:
    sys.exit(23)
if name == 'cargo' and 'macos_native' in args:
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
                self.assertEqual(len(native), 2)
                self.assertIsNone(native[0]['skills'])
                self.assertEqual(native[1]['skills'], '1')

    def test_coverage_environment_failure_cannot_run_uninstrumented_tests(self):
        result, calls = self.run_script('test-coverage.sh')
        self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
        self.assertFalse(any(c['name'] == 'cargo' and c['args'][0] == 'test' for c in calls))


if __name__ == '__main__':
    unittest.main()
