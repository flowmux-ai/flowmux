#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise actual local Git pushes; stub only the expensive build commands."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
STUB = '''
import json, os, pathlib, subprocess, sys
name = pathlib.Path(sys.argv[0]).name
step = name + ' ' + ' '.join(sys.argv[1:])
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps({'step': step, 'git_dir': os.environ.get('GIT_DIR')}) + '\\n')
if step == os.environ.get('FAIL_STEP'):
    sys.exit(23)
if step == 'npm run build' and os.environ.get('MUTATE') == 'dist':
    pathlib.Path('dist/new.js').write_text('uncommitted generated asset')
if name == 'cargo' and sys.argv[1] == 'clippy':
    if os.environ.get('MUTATE') == 'head':
        subprocess.run(['git', 'commit', '--allow-empty', '-qm', 'concurrent commit'], check=True)
    if os.environ.get('MUTATE') == 'dirty':
        pathlib.Path('source.rs').write_text('concurrent edit')
'''


class PushTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='fm-push-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.repo = self.root / 'repo'
        self.repo.mkdir()
        self.remote = self.root / 'remote.git'
        self.calls = self.root / 'calls'
        self.env = {k: v for k, v in os.environ.items()
                    if not k.startswith('GIT_')}
        self.env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull,
                        CALLS=str(self.calls), PYTHONDONTWRITEBYTECODE='1')
        binaries = self.root / 'bin'
        binaries.mkdir()
        self.env['PATH'] = str(binaries) + os.pathsep + self.env['PATH']
        for name in ('cargo', 'npm'):
            target = binaries / name
            target.write_text(f'#!{sys.executable}\n' + STUB)
            target.chmod(0o755)
        for name in ('.githooks/pre-push', 'scripts/check-pre-push.sh'):
            target = self.repo / name
            target.parent.mkdir(exist_ok=True)
            shutil.copy2(ROOT / name, target)
        (self.repo / '.githooks/pre-push').chmod(0o755)
        (self.repo / 'scripts/test-agent-unit.py').write_text(STUB)
        dist = self.repo / 'editor/flowmux-editor-web/dist'
        dist.mkdir(parents=True)
        (dist / 'bundle.js').write_text('committed bundle')
        (self.repo / 'source.rs').write_text('initial')
        self.git('init', '-q', '-b', 'main')
        self.git('config', 'user.name', 'Hook test')
        self.git('config', 'user.email', 'hook@example.invalid')
        self.git('config', 'core.hooksPath', '.githooks')
        self.git('add', '.')
        self.git('commit', '-qm', 'initial')
        self.base = self.git('rev-parse', 'HEAD').stdout.strip()
        self.git('init', '-q', '--bare', str(self.remote))
        # Seed only the disposable remote without running the hook under test.
        self.git('-c', 'core.hooksPath=/dev/null', 'push', str(self.remote), 'HEAD:main')
        (self.repo / 'source.rs').write_text('changed')
        self.git('commit', '-qam', 'change')
        self.head = self.git('rev-parse', 'HEAD').stdout.strip()

    def git(self, *args, check=True, **env):
        return subprocess.run(['git', *args], cwd=self.repo,
                              env=dict(self.env, **env), text=True,
                              capture_output=True, check=check, timeout=30)

    def push(self, *refs, **env):
        return self.git('push', str(self.remote), *(refs or ('HEAD:main',)), check=False, **env)

    def steps(self):
        if not self.calls.exists():
            return []
        return [json.loads(line)['step'] for line in self.calls.read_text().splitlines()]

    def assert_blocked(self, result, message=None):
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        if message:
            self.assertIn(message, result.stderr)
        actual = self.git('--git-dir=' + str(self.remote), 'rev-parse', 'main').stdout.strip()
        self.assertEqual(actual, self.base, 'rejected push changed the remote')

    def test_clean_push_runs_native_checks_and_clears_git_environment(self):
        result = self.push(GIT_DIR=str(self.repo / '.git'))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.steps(), ['cargo fmt --all -- --check',
            'cargo clippy --workspace --all-targets --locked -- -D warnings',
            'test-agent-unit.py --all'])
        self.assertTrue(all(json.loads(line)['git_dir'] is None
                            for line in self.calls.read_text().splitlines()))

    def test_each_validation_failure_blocks_push(self):
        for step in ['cargo fmt --all -- --check',
                     'cargo clippy --workspace --all-targets --locked -- -D warnings',
                     'test-agent-unit.py --all', 'npm ci', 'npm run test:coverage',
                     'npm run build', 'npm run verify']:
            with self.subTest(step=step):
                self.assert_blocked(self.push('HEAD:new-branch', FAIL_STEP=step))
                self.assertEqual(self.steps()[-1], step, 'continued after failure')

    def test_dirty_tracked_and_untracked_files_block_before_checks(self):
        for name in ('source.rs', 'untracked.rs'):
            with self.subTest(name=name):
                path = self.repo / name
                previous = path.read_text() if path.exists() else None
                path.write_text('uncommitted')
                self.assert_blocked(self.push(), 'commit or stash')
                self.assertEqual(self.steps(), [])
                if previous is None:
                    path.unlink()
                else:
                    path.write_text(previous)

    def test_another_commit_in_a_multi_ref_push_blocks_all_refs(self):
        self.git('branch', 'old', self.base)
        self.assert_blocked(self.push('HEAD:main', 'old:other'), 'only HEAD')
        self.assertEqual(self.steps(), [])

    def test_editor_changes_and_new_refs_run_web_checks(self):
        result = self.push('HEAD:new-branch')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('npm run verify', self.steps())
        self.calls.unlink()
        (self.repo / 'editor/flowmux-editor-web/dist/bundle.js').write_text('updated')
        self.git('commit', '-qam', 'editor change')
        result = self.push()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('npm run verify', self.steps())

    def test_new_generated_assets_block_push(self):
        self.assert_blocked(self.push('HEAD:new-branch', MUTATE='dist'), 'regenerated dist')

    def test_concurrent_changes_block_push(self):
        for change in ('head', 'dirty'):
            with self.subTest(change=change):
                self.assert_blocked(self.push(MUTATE=change))
                self.git('reset', '--hard', self.head)

    def test_ref_deletion_needs_no_checks_even_with_local_edits(self):
        (self.repo / 'source.rs').write_text('dirty')
        result = self.push(':main')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.steps(), [])

    def test_annotated_tag_at_head_is_validated(self):
        self.git('tag', '-am', 'release', 'v-test')
        result = self.push('refs/tags/v-test')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('test-agent-unit.py --all', self.steps())


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
