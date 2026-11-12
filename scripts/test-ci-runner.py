#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check gate failure propagation and isolation without launching a desktop."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
STUB = r'''
import json, os, pathlib, sys
name = pathlib.Path(sys.argv[0]).stem
args = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps({'name': name, 'args': args,
        'socket': os.environ.get('FLOWMUX_SOCKET_PATH'),
        'tmp': os.environ.get('TMPDIR'),
        'runtime': os.environ.get('FLOWMUX_RUNTIME_DIR'),
        'desktop': os.environ.get('XDG_CURRENT_DESKTOP'),
        'session_desktop': os.environ.get('XDG_SESSION_DESKTOP'),
        'skills': os.environ.get('FLOWMUX_SKILLS_SMOKE_ONLY'),
        'boot_restore': os.environ.get('FLOWMUX_RESTORED_ROOTS_SMOKE_ONLY'),
        'ssh_preview': os.environ.get('FLOWMUX_SSH_PREVIEW_SMOKE_ONLY'),
        'keyboard': os.environ.get('FLOWMUX_EDITOR_FOCUS_SMOKE_ONLY'),
        'agents': os.environ.get('FLOWMUX_AGENT_SMOKE_ONLY'),
        'session_paths': os.environ.get('FLOWMUX_SESSION_PATHS_SMOKE_ONLY'),
        'session_flow': os.environ.get('FLOWMUX_SESSION_FLOW_SMOKE_ONLY'),
        'renderer': os.environ.get('GSK_RENDERER')}) + '\n')
if name == 'uname': print(os.environ.get('TEST_OS', 'Linux'))
if name == 'xvfb-run': sys.exit(int(os.environ.get('TEST_EXIT', '0')))
if name == 'cargo' and args[:2] == ['test', '--workspace']:
    sys.exit(int(os.environ.get('WORKSPACE_TEST_EXIT', '0')))
if name == 'python3' and args and args[0] == os.environ.get('LINUX_GUI_FAIL'):
    sys.exit(23)
if name == 'python3' and args == ['scripts/test-agent-unit.py']:
    sys.exit(int(os.environ.get('AGENT_UNIT_EXIT', '0')))
if name == 'cargo' and args[:3] == ['test', '-p', 'flowmux-md-viewer']:
    if os.environ.get('MARKDOWN_MARKER', '1') == '1':
        print('MACOS_MARKDOWN_NATIVE_OK')
    sys.exit(int(os.environ.get('MARKDOWN_EXIT', '0')))
if name == 'cargo' and args[:3] == ['test', '-p', 'flowmux-notify']:
    if os.environ.get('NOTIFICATIONS_MARKER', '1') == '1':
        print('MACOS_NATIVE_DOCK_BADGE_OK')
    sys.exit(int(os.environ.get('NOTIFICATIONS_EXIT', '0')))
if name == 'python3' and args and args[0] == 'scripts/test-macos-webdriver.py':
    if os.environ.get('WEBDRIVER_MARKER', '1') == '1':
        print('MACOS_WEBDRIVER_OK')
    sys.exit(int(os.environ.get('WEBDRIVER_EXIT', '0')))
if name == 'python3' and args == ['scripts/test-update-macos-dmg.py']:
    sys.exit(int(os.environ.get('DMG_UPDATE_EXIT', '0')))
if name == 'cargo' and 'flowmux-state' in args and 'flowmux-procmon' in args:
    sys.exit(int(os.environ.get('NATIVE_STATE_EXIT', '0')))
if name == 'cargo' and 'macos_native' in args:
    for flag, prefix, marker in [('FLOWMUX_SESSION_FLOW_SMOKE_ONLY', 'SESSION_FLOW', 'MACOS_SESSION_FLOW_LAYOUT_OK'),
                                  ('FLOWMUX_BROWSER_DEADLINE_SMOKE_ONLY', 'BROWSER_DEADLINE', 'MACOS_BROWSER_SCRIPT_DEADLINE_OK'),
                                  ('FLOWMUX_OFFICE_STRESS_SMOKE_ONLY', 'OFFICE_STABILITY', 'OFFICE_STABILITY_OK cycles=12'),
                                  ('FLOWMUX_BROWSER_CHROME_SMOKE_ONLY', 'BROWSER_CHROME', 'BROWSER_CHROME_LAYOUT_OK'),
                                  ('FLOWMUX_RESTORED_ROOTS_SMOKE_ONLY', 'BOOT_RESTORE', 'MACOS_BOOT_RESTORE_POLICY_OK'),
                                  ('FLOWMUX_SSH_PREVIEW_SMOKE_ONLY', 'SSH_PREVIEW', 'MACOS_SSH_PREVIEW_EXPIRY_OK'),
                                  ('FLOWMUX_IMAGE_VIEWER_SMOKE_ONLY', 'IMAGE_VIEWER', 'MACOS_IMAGE_LOTTIE_VIEWER_OK'),
                                  ('FLOWMUX_EDITOR_RECOVERY_SMOKE_ONLY', 'EDITOR_RECOVERY', 'MACOS_EDITOR_RENDERER_RETRY_OK'),
                                  ('FLOWMUX_PALETTE_RELEASE_SMOKE_ONLY', 'PALETTE_RELEASE', 'MACOS_PALETTE_RELEASE_OK')]:
        if os.environ.get(flag):
            if os.environ.get(prefix + '_MARKER', '1') == '1':
                print(marker)
            sys.exit(int(os.environ.get(prefix + '_EXIT', '0')))
    if os.environ.get('FLOWMUX_NOTIFICATION_CLEANUP_SMOKE_ONLY'):
        if os.environ.get('CLEANUP_MARKER', '1') == '1':
            print('MACOS_NOTIFICATION_CLEANUP_OK')
        sys.exit(int(os.environ.get('CLEANUP_EXIT', '0')))
    if os.environ.get('FLOWMUX_SESSION_PATHS_SMOKE_ONLY'):
        if os.environ.get('SESSION_PATHS_MARKER', '1') == '1':
            print('MACOS_SESSION_PATHS_OK')
        sys.exit(int(os.environ.get('SESSION_PATHS_EXIT', '0')))
    if os.environ.get('FLOWMUX_EDITOR_FOCUS_SMOKE_ONLY'):
        if os.environ.get('KEYBOARD_MARKER', '1') == '1':
            print('MACOS_FOUR_PANE_EDITING_KEYS_OK')
        sys.exit(int(os.environ.get('KEYBOARD_TEST_EXIT', '0')))
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
    @unittest.skipUnless(sys.platform == 'linux', 'Linux subreaper ownership')
    def test_phase_reaps_exited_orphans_while_command_is_running(self):
        job = """
import os, pathlib, sys, time
root = pathlib.Path(sys.argv[1])
parent = os.fork()
if parent == 0:
    child = os.fork()
    if child == 0:
        time.sleep(.1)
        (root / 'completed').touch()
        os._exit(0)
    (root / 'child.pid').write_text(str(child))
    os._exit(0)
os.waitpid(parent, 0)
child = int((root / 'child.pid').read_text())
deadline = time.monotonic() + 2
while True:
    try:
        os.kill(child, 0)
    except ProcessLookupError:
        break
    assert time.monotonic() < deadline, 'orphan zombie retained until phase exit'
    time.sleep(.01)
assert (root / 'completed').exists(), 'live orphan was killed before completing'
sys.exit(27)
"""
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run([
                sys.executable, str(ROOT / 'scripts/ci-phase.py'), directory, 'live-reaping', '10',
                sys.executable, '-c', job, directory,
            ], capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 27, result.stdout + result.stderr)

    @unittest.skipUnless(sys.platform == 'linux', 'Linux detached GUI fixtures')
    def test_phase_reaps_detached_descendants_without_touching_other_processes(self):
        job = """
import pathlib, subprocess, sys, time
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                         start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
pathlib.Path(sys.argv[1]).write_text(str(child.pid))
if sys.argv[2] == 'timeout':
    time.sleep(60)
sys.exit(27 if sys.argv[2] == 'failed' else 0)
"""
        protected = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
        try:
            for mode, expected in [('passed', 0), ('failed', 27), ('timeout', 124)]:
                with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                    info = Path(directory) / 'child.pid'
                    try:
                        result = subprocess.run([
                            sys.executable, str(ROOT / 'scripts/ci-phase.py'), directory, mode,
                            '.5' if mode == 'timeout' else '10',
                            sys.executable, '-c', job, str(info), mode,
                        ], capture_output=True, text=True, timeout=10)
                        self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                        self.assertIsNone(protected.poll(), 'cleanup reached an unrelated process')
                        with self.assertRaises(ProcessLookupError, msg='detached fixture survived'):
                            os.kill(int(info.read_text()), 0)
                    finally:
                        if info.exists():
                            try:
                                os.kill(int(info.read_text()), signal.SIGKILL)
                            except ProcessLookupError:
                                pass
        finally:
            protected.kill()
            protected.wait(timeout=5)

    @unittest.skipUnless(sys.platform == 'linux', 'Linux nested phase ownership')
    def test_outer_timeout_reaps_children_of_an_interrupted_inner_phase(self):
        job = """
import pathlib, subprocess, sys, time
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'],
                         start_new_session=True)
pathlib.Path(sys.argv[1]).write_text(str(child.pid))
time.sleep(60)
"""
        with tempfile.TemporaryDirectory() as directory:
            info = Path(directory) / 'child.pid'
            phase = [sys.executable, str(ROOT / 'scripts/ci-phase.py')]
            try:
                result = subprocess.run(
                    phase + [directory, 'outer', '.5'] + phase + [directory, 'inner', '30',
                        sys.executable, '-c', job, str(info)],
                    capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 124, result.stdout + result.stderr)
                self.assertEqual(json.loads((Path(directory) / 'outer.json').read_text())['status'], 'timeout')
                with self.assertRaises(ProcessLookupError):
                    os.kill(int(info.read_text()), 0)
            finally:
                if info.exists():
                    try:
                        os.kill(int(info.read_text()), signal.SIGKILL)
                    except ProcessLookupError:
                        pass

    def test_native_timeout_samples_only_owned_tests_and_preserves_failure(self):
        spec = importlib.util.spec_from_file_location('phase', ROOT / 'scripts/ci-phase.py')
        phase = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(phase)
        for error in (None, OSError('sample unavailable'), subprocess.TimeoutExpired('sample', 10)):
            with self.subTest(error=error), tempfile.TemporaryDirectory() as directory:
                process = Mock(pid=123)
                process.wait.side_effect = [subprocess.TimeoutExpired('cargo', 1), 0]
                with contextlib.redirect_stdout(io.StringIO()), \
                     patch.object(sys, 'argv', ['ci-phase.py', directory, 'native', '1', 'cargo']), \
                     patch.object(sys, 'platform', 'darwin'), \
                     patch.object(phase.signal, 'signal'), \
                     patch.object(phase.os, 'killpg') as kill, \
                     patch.object(phase.subprocess, 'Popen', return_value=process), \
                     patch.object(phase.subprocess, 'check_output', return_value=(
                         '123 123 /bin/cargo\n124 123 /build/macos_native-abc\n'
                         '125 123 /bin/sh\n126 999 /other/macos_native-def\n')), \
                     patch.object(phase.subprocess, 'run', side_effect=error) as sample:
                    self.assertEqual(phase.main(), 124)
                sample.assert_called_once_with(
                    ['/usr/bin/sample', '124', '3', '1', '-file',
                     str(Path(directory) / 'native.124.sample')], timeout=10, check=True)
                kill.assert_called_once_with(123, signal.SIGKILL)
                receipt = json.loads((Path(directory) / 'native.json').read_text())
                self.assertEqual((receipt['status'], receipt['exit_code']), ('timeout', 124))
                self.assertEqual('diagnostic_error' in receipt, error is not None)

    def test_phase_deadlines_exit_codes_and_descendant_cleanup(self):
        job = """
import os, pathlib, signal, subprocess, sys, time
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
pathlib.Path(sys.argv[1]).write_text(str(child.pid))
if sys.argv[2] in ('passed', 'failed'):
    sys.exit(0 if sys.argv[2] == 'passed' else 27)
if sys.argv[2] == 'crashed':
    os.kill(os.getpid(), signal.SIGKILL)
time.sleep(60)
"""
        for mode, expected in [('passed', 0), ('failed', 27), ('timeout', 124),
                               ('terminate', 143), ('interrupt', 130), ('crashed', 137)]:
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                info = root / 'child.pid'
                process = subprocess.Popen([
                    sys.executable, str(ROOT / 'scripts/ci-phase.py'), directory, mode,
                    '0.5' if mode == 'timeout' else '10',
                    sys.executable, '-c', job, str(info), mode,
                ], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
                try:
                    deadline = time.monotonic() + 10
                    while not info.exists() or not info.read_text():
                        self.assertIsNone(process.poll())
                        self.assertLess(time.monotonic(), deadline)
                        time.sleep(.01)
                    if mode in ('terminate', 'interrupt'):
                        process.send_signal(signal.SIGTERM if mode == 'terminate' else signal.SIGINT)
                    output, _ = process.communicate(timeout=10)
                    self.assertEqual(process.returncode, expected, output)
                    receipt = json.loads((root / (mode + '.json')).read_text())
                    self.assertEqual(receipt['exit_code'], expected)
                    self.assertEqual(receipt['status'],
                                     'interrupted' if mode in ('terminate', 'interrupt') else
                                     'failed' if mode == 'crashed' else mode)
                    self.assertLess(receipt['elapsed_seconds'], 5)
                    self.assertIn('CI_PHASE_START=' + mode, output)
                    self.assertIn('CI_PHASE_RESULT=', output)
                    child = int(info.read_text())
                    deadline = time.monotonic() + 2
                    while True:
                        try:
                            os.kill(child, 0)
                        except ProcessLookupError:
                            break
                        self.assertLess(time.monotonic(), deadline, 'phase descendant survived')
                        time.sleep(.02)
                finally:
                    if process.poll() is None:
                        process.terminate()
                    process.communicate(timeout=10)
                    if info.exists() and info.read_text():
                        try:
                            os.kill(int(info.read_text()), signal.SIGKILL)
                        except ProcessLookupError:
                            pass

    def run_script(self, script, *args, **extra):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ['uname', 'rustc', 'rustup', 'cargo', 'pkg-config', 'xvfb-run', 'python3']:
                stub = root / name
                stub.write_text(f'#!{sys.executable} -S\n' + STUB)
                stub.chmod(0o755)
            # Skip a second interpreter per phase; real timeout/cleanup tests stay above.
            (root / 'python3.py').write_text(STUB)
            (root / 'python3').write_text(
                '#!/bin/sh\n'
                'if [ "$1" = scripts/ci-phase.py ]; then shift 4; exec "$@"; fi\n'
                f'exec {shlex.quote(sys.executable)} -S {shlex.quote(str(root / "python3.py"))} "$@"\n')
            env = dict(os.environ, PATH=f'{root}:{os.environ["PATH"]}',
                       CALLS=str(root / 'calls'), CARGO_TARGET_DIR=str(root / 'target'),
                       FLOWMUX_SOCKET_PATH='/do/not/use/live.sock', **extra)
            result = subprocess.run(['bash', str(ROOT / 'scripts' / script), *args],
                                    env=env, capture_output=True, text=True)
            calls = [json.loads(line) for line in (root / 'calls').read_text().splitlines()]
            if script == 'test-linux.sh':
                saved = (root / 'target/ci/linux/tests.jsonl').read_text().splitlines()
                logged = [line.split('=', 1)[1] for line in result.stdout.splitlines()
                          if line.startswith('CI_TEST_RESULT=')]
                self.assertEqual(saved, logged, 'uploaded timings must match the job log')
            return result, calls

    def test_linux_failure_preserves_isolation_without_coverage_or_retries(self):
        result, calls = self.run_script('test-ci.sh', 'linux', TEST_EXIT='17',
                                       XDG_CURRENT_DESKTOP='ubuntu:GNOME', XDG_SESSION_DESKTOP='ubuntu')
        self.assertEqual(result.returncode, 17, result.stdout + result.stderr)
        desktop = [call for call in calls if call['name'] == 'xvfb-run']
        self.assertEqual(len(desktop), 1, 'a failed test must not be retried')
        self.assertIsNone(desktop[0]['socket'])
        self.assertEqual(desktop[0]['desktop'], '')
        self.assertEqual(desktop[0]['session_desktop'], '')
        self.assertTrue(desktop[0]['runtime'].startswith('/tmp/fm-ci.'))
        self.assertFalse(Path(desktop[0]['tmp']).exists(), 'temporary state must be removed')
        self.assertIn('scripts/test-linux.sh', desktop[0]['args'])
        self.assertIn('regression', desktop[0]['args'])
        self.assertFalse(any('llvm-cov' in c['args'] for c in calls))

    def test_macos_pipeline_does_not_hide_native_failure(self):
        result, calls = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin', TEST_EXIT='19')
        self.assertEqual(result.returncode, 19, result.stdout + result.stderr)
        self.assertEqual(sum('macos_native' in c['args'] for c in calls), 1)

    def test_macos_parity_gates_preserve_failures_and_require_native_markers(self):
        for phase in ('BOOT_RESTORE', 'SSH_PREVIEW', 'IMAGE_VIEWER', 'BROWSER_CHROME', 'PALETTE_RELEASE', 'EDITOR_RECOVERY', 'OFFICE_STABILITY', 'BROWSER_DEADLINE'):
            for exit_code, marker, expected in [(41, '1', 41), (0, '0', 1)]:
                with self.subTest(phase=phase, marker=marker):
                    result, _ = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
                        **{phase + '_EXIT': str(exit_code), phase + '_MARKER': marker})
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
        for phase in ('NATIVE_STATE', 'DMG_UPDATE'):
            with self.subTest(phase=phase):
                result, _ = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
                    **{phase + '_EXIT': '41'})
                self.assertEqual(result.returncode, 41, result.stdout + result.stderr)

    def test_macos_skill_gate_runs_and_propagates_failure(self):
        for exit_code in (0, 21):
            with self.subTest(exit_code=exit_code):
                result, calls = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
                                                SKILLS_TEST_EXIT=str(exit_code))
                self.assertEqual(result.returncode, exit_code, result.stdout + result.stderr)
                native = [c for c in calls if 'macos_native' in c['args']]
                self.assertEqual(len(native), 15 if exit_code == 0 else 5)
                self.assertIsNone(native[0]['skills'])
                self.assertEqual(native[4]['skills'], '1')

    def test_macos_keyboard_gate_requires_success_and_completion_marker(self):
        for exit_code, marker, expected in [(0, '1', 0), (29, '1', 29), (0, '0', 1)]:
            with self.subTest(exit_code=exit_code, marker=marker):
                result, calls = self.run_script(
                    'test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
                    KEYBOARD_TEST_EXIT=str(exit_code), KEYBOARD_MARKER=marker)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                native = [c for c in calls if 'macos_native' in c['args']]
                self.assertEqual(len(native), 15 if expected == 0 else 4)
                self.assertEqual(native[3]['keyboard'], '1')
                self.assertIsNone(native[3]['socket'])

    def test_macos_gate_uses_cairo_despite_inherited_renderer(self):
        for renderer in ('', 'gl'):
            with self.subTest(renderer=renderer):
                result, calls = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
                                                GSK_RENDERER=renderer)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                native = [c for c in calls if 'macos_native' in c['args']]
                self.assertEqual(len(native), 15)
                self.assertTrue(all(c['renderer'] == 'cairo' for c in native))
                preview = next(c for c in native if c['ssh_preview'] == '1')
                self.assertTrue(preview['tmp'].endswith('/macos-native-long-temporary-directory'))
                self.assertFalse(Path(preview['tmp']).exists())
                build = next(i for i, c in enumerate(calls) if c['name'] == 'cargo' and c['args'][0] == 'build')
                tests = next(i for i, c in enumerate(calls) if c['name'] == 'cargo' and c['args'][0] == 'test')
                self.assertLess(build, tests, 'cold compilation has its own build budget')

    def test_macos_session_paths_gate_requires_success_and_marker(self):
        for exit_code, marker, expected in [(0, '1', 0), (31, '1', 31), (0, '0', 1)]:
            with self.subTest(exit_code=exit_code, marker=marker):
                result, calls = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
                    SESSION_PATHS_EXIT=str(exit_code), SESSION_PATHS_MARKER=marker)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                native = [c for c in calls if c['session_paths'] == '1']
                self.assertEqual(len(native), 1)
                self.assertIsNone(native[0]['socket'])

    def test_macos_session_flow_gate_requires_success_and_marker(self):
        for exit_code, marker, expected in [(0, '1', 0), (31, '1', 31), (0, '0', 1)]:
            with self.subTest(exit_code=exit_code, marker=marker):
                result, calls = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
                    SESSION_FLOW_EXIT=str(exit_code), SESSION_FLOW_MARKER=marker)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                native = [c for c in calls if c['session_flow'] == '1']
                self.assertEqual(len(native), 1)
                self.assertIsNone(native[0]['socket'])

    def test_macos_markdown_gate_requires_success_and_native_marker(self):
        for exit_code, marker, expected in [(0, '1', 0), (37, '1', 37), (0, '0', 1)]:
            with self.subTest(exit_code=exit_code, marker=marker):
                result, calls = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
                    MARKDOWN_EXIT=str(exit_code), MARKDOWN_MARKER=marker)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                markdown = [c for c in calls if 'flowmux-md-viewer' in c['args']]
                self.assertEqual(len(markdown), 1)
                self.assertIsNone(markdown[0]['socket'])

    def test_macos_webdriver_gate_requires_success_and_marker(self):
        for exit_code, marker, expected in [(0, '1', 0), (43, '1', 43), (0, '0', 1)]:
            with self.subTest(exit_code=exit_code, marker=marker):
                result, calls = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
                    WEBDRIVER_EXIT=str(exit_code), WEBDRIVER_MARKER=marker)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                driver = [c for c in calls if 'scripts/test-macos-webdriver.py' in c['args']]
                self.assertEqual(len(driver), 1)
                self.assertIsNone(driver[0]['socket'])

    def test_macos_notification_gates_require_success_and_markers(self):
        for phase in ('NOTIFICATIONS', 'CLEANUP'):
            for exit_code, marker, expected in [(0, '1', 0), (39, '1', 39), (0, '0', 1)]:
                with self.subTest(phase=phase, exit_code=exit_code, marker=marker):
                    result, _ = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
                        **{phase + '_EXIT': str(exit_code), phase + '_MARKER': marker})
                    self.assertEqual(result.returncode, expected, result.stdout + result.stderr)

    def test_macos_agent_gates_run_and_propagate_failure(self):
        for variable in ('AGENT_UNIT_EXIT', 'AGENT_GUI_EXIT'):
            with self.subTest(variable=variable):
                result, calls = self.run_script('test-ci.sh', 'macos', 'extended', TEST_OS='Darwin',
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

    def test_linux_workspace_failure_stops_before_build_and_gui(self):
        result, calls = self.run_script('test-linux.sh', WORKSPACE_TEST_EXIT='23')
        self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
        self.assertFalse(any(c['name'] == 'cargo' and c['args'][0] == 'build' for c in calls))
        self.assertFalse(any('scripts/test-browser-control.py' in c['args'] for c in calls))
        self.assertIn('"test":"workspace"', result.stdout)
        self.assertIn('"exit_code":23}', result.stdout)

    def test_linux_suites_keep_functional_tests_and_only_extend_stress(self):
        expected = {
            'test-gui-harness.py', 'test-ssh-workspace-fixture.py', 'test-browser-control.py',
            'test-browser-source-gui.py',
            'test-browser-bookmarks-gui.py', 'test-browser-import-gui.py',
            'test-skills-gui.py', 'test-notifications-gui.py',
            'test-team-pingpong-gui.py', 'test-team-recovery-gui.py',
            'test-team-reuse-gui.py', 'test-team-usage-gui.py',
            'test-session-save-gui.py', 'test-session-flow-gui.py',
            'test-pty-spawn-failure-gui.py', 'test-terminal-read-screen-gui.py',
            'test-terminal-image-links-gui.py', 'test-ipc-recovery-gui.py',
            'test-polling-delay-gui.py', 'test-editor-io-gui.py',
            'test-editor-renderer-gui.py', 'test-lazy-restore-gui.py',
            'ssh-workspace-fixture.py',
        }
        for suite in ('regression', 'extended'):
            with self.subTest(suite=suite):
                result, calls = self.run_script('test-linux.sh', suite, '--', '--nocapture')
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                scripts = {Path(c['args'][0]).name for c in calls if c['name'] == 'python3'}
                self.assertEqual(scripts, expected | ({'test-idle-stability-gui.py'}
                                                     if suite == 'extended' else set()))
                cargo = [c['args'] for c in calls if c['name'] == 'cargo']
                self.assertEqual(cargo, [['test', '--workspace', '--locked', '--no-fail-fast',
                                          '--', '--nocapture'], ['build', '--workspace', '--locked']])
                timings = [json.loads(line.split('=', 1)[1]) for line in result.stdout.splitlines()
                           if line.startswith('CI_TEST_RESULT=')]
                self.assertEqual(len(timings), 27 if suite == 'extended' else 25)
                self.assertEqual(len({row['test'] for row in timings}), len(timings))
                self.assertTrue(all(row['exit_code'] == 0 and row['elapsed_seconds'] >= 0
                                    for row in timings))
                session_flow = next(c for c in calls if c['args'][0] == 'scripts/test-session-flow-gui.py')
                self.assertEqual(session_flow['tmp'], '/tmp', 'GUI evidence must survive gate cleanup')

    def test_linux_gui_failure_stops_the_suite(self):
        result, calls = self.run_script('test-linux.sh',
                                       LINUX_GUI_FAIL='scripts/test-session-flow-gui.py')
        self.assertEqual(result.returncode, 23, result.stdout + result.stderr)
        self.assertFalse(any('scripts/ssh-workspace-fixture.py' in c['args'] for c in calls))
        self.assertIn('"test":"session-flow"', result.stdout)
        self.assertIn('"exit_code":23}', result.stdout)

    def test_macos_default_runs_functional_suites_without_office_stress(self):
        result, calls = self.run_script('test-ci.sh', 'macos', TEST_OS='Darwin')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        native = [c for c in calls if 'macos_native' in c['args']]
        self.assertEqual(len(native), 14)
        self.assertNotIn('office-stability.log', result.stdout)
        self.assertIn('MACOS_BROWSER_SCRIPT_DEADLINE_OK', result.stdout)


if __name__ == '__main__':
    unittest.main()
