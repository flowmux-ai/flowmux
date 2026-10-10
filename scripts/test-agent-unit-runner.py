#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check isolated test runner environment, failure handling and cleanup."""
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]


class UnitRunnerTests(unittest.TestCase):
    def test_all_mode_keeps_isolation_and_default_mode_keeps_its_packages(self):
        sys.dont_write_bytecode = True
        spec = importlib.util.spec_from_file_location('agent_unit', ROOT / 'scripts/test-agent-unit.py')
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        for all_packages in (False, True):
            with self.subTest(all_packages=all_packages), patch.object(runner.subprocess, 'run') as run:
                run.return_value.returncode = 23
                with patch.dict(os.environ, {'FLOWMUX_SOCKET_PATH': '/must/not/use.sock'}):
                    self.assertEqual(runner.run_tests(all_packages), 23)
                command = run.call_args.args[0]
                self.assertEqual(command[:3], ['cargo', 'test', '--locked'])
                self.assertEqual('-p' in command, not all_packages)
                env = run.call_args.kwargs['env']
                self.assertNotIn('FLOWMUX_SOCKET_PATH', env)
                self.assertEqual(env['XDG_RUNTIME_DIR'], env['FLOWMUX_RUNTIME_DIR'])
                self.assertFalse(Path(env['FLOWMUX_RUNTIME_DIR']).exists())

    def test_linux_service_failure_is_not_ignored(self):
        sys.dont_write_bytecode = True
        spec = importlib.util.spec_from_file_location('agent_unit', ROOT / 'scripts/test-agent-unit.py')
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        with patch.object(runner.sys, 'platform', 'linux'), patch.object(runner.subprocess, 'run') as run:
            run.side_effect = [subprocess.CompletedProcess([], 23), subprocess.CompletedProcess([], 5)]
            self.assertEqual(runner.main(all_packages=True), 23)
            command = run.call_args_list[0].args[0]
            self.assertEqual(command[:5], ['systemd-run', '--user', '--wait', '--pipe', '--collect'])
            self.assertEqual(command[-2:], ['--worker', '--all'])
            self.assertIn('--property=RuntimeMaxSec=960', command)
            unit = next(arg.removeprefix('--unit=') for arg in command if arg.startswith('--unit='))
            self.assertEqual(run.call_args.args[0], ['systemctl', '--user', 'stop', unit])

    def test_linux_timeout_cleans_service_and_forwards_only_build_environment(self):
        spec = importlib.util.spec_from_file_location('agent_unit', ROOT / 'scripts/test-agent-unit.py')
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        with patch.object(runner.sys, 'platform', 'linux'), patch.object(runner.subprocess, 'run') as run:
            run.side_effect = [subprocess.TimeoutExpired('systemd-run', 970),
                               subprocess.CompletedProcess([], 0)]
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': '/tmp/build cache',
                                        'CARGO_PROFILE_TEST_DEBUG': '0',
                                        'FLOWMUX_SOCKET_PATH': '/must/not/use.sock',
                                        'CARGO_REGISTRIES_PRIVATE_TOKEN': 'must-not-forward'}):
                with self.assertRaises(subprocess.TimeoutExpired):
                    runner.main()
            command = run.call_args_list[0].args[0]
            self.assertIn('--setenv=CARGO_TARGET_DIR=/tmp/build cache', command)
            self.assertIn('--setenv=CARGO_PROFILE_TEST_DEBUG=0', command)
            self.assertFalse(any('must-not' in arg or '/must/not' in arg for arg in command))
            self.assertEqual(run.call_args.args[0][:3], ['systemctl', '--user', 'stop'])

    def test_linux_cleanup_failure_is_not_reported_as_success(self):
        spec = importlib.util.spec_from_file_location('agent_unit', ROOT / 'scripts/test-agent-unit.py')
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        with patch.object(runner.sys, 'platform', 'linux'), patch.object(runner.subprocess, 'run') as run:
            run.side_effect = [subprocess.CompletedProcess([], 0),
                               subprocess.CompletedProcess([], 1, stderr='manager unavailable')]
            with self.assertRaisesRegex(RuntimeError, 'cleanup failed: manager unavailable'):
                runner.main()


if __name__ == '__main__':
    unittest.main()
