#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify usage collection, initial routing and shared quota holds in isolated GUI panes."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('recovery', REPO / 'scripts/test-team-recovery-gui.py')
recovery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(recovery)
fixture = recovery.fixture

APP_SERVER = r'''
if len(sys.argv) > 1 and sys.argv[1] == 'app-server':
    for line in sys.stdin:
        request = json.loads(line)
        if 'id' not in request:
            continue
        usage = json.loads((root / 'usage-behavior.json').read_text())
        result = {}
        if request['id'] == 1:
            result = {'rateLimits': {'primary': {'usedPercent': usage.get('used', 0),
                      'resetsAt': usage.get('reset'), 'windowDurationMins': 300}}}
        response = {'id': request['id'], 'result': result}
        if request['id'] == 1 and usage.get('error'):
            response = {'id': 1, 'error': {'message': 'Fixture usage unavailable'}}
        print(json.dumps(response), flush=True)
    raise SystemExit(0)
'''
PROVIDER = recovery.PROVIDER.replace('prompt = sys.argv[-1]', APP_SERVER + '\nprompt = sys.argv[-1]', 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default=str(REPO / 'target/debug/flowmux'))
    parser.add_argument('--cli', default=str(REPO / 'target/debug/flowmuxctl'))
    parser.add_argument('--protected-pid', type=int, action='append', default=[])
    args = parser.parse_args()
    h = fixture.Harness(args)
    for key in list(h.env):
        if key.startswith('CODEX_'):
            h.env.pop(key)
    h.env.update(CODEX_HOME=str(h.root / 'codex'), CLAUDE_CONFIG_DIR=str(h.root / 'claude'),
                 GEMINI_CLI_HOME=str(h.root / 'gemini'))
    native = h.root / 'providers'
    native.mkdir()
    for agent in ('codex', 'claude'):
        path = native / agent
        path.write_text('#!' + sys.executable + '\n' + PROVIDER)
        path.chmod(0o700)
    h.env['PATH'] = str(native) + os.pathsep + str(Path(args.cli).parent) + os.pathsep + os.defpath
    (h.root / 'behavior.json').write_text('{}')
    options = h.root / 'config/flowmux/options.json'
    options.parent.mkdir(parents=True)
    options.write_text(json.dumps(dict(usage_bar_enabled=False, default_shell='/bin/sh')))
    usage = h.root / 'usage-behavior.json'
    usage.write_text(json.dumps(dict(used=100, reset=int(time.time()) + 3600)))
    task = h.root / 'task.txt'
    task.write_text('Read-only usage routing fixture.')
    try:
        h.start_display()
        process, socket = h.window('usage-routing')
        directory = h.root / 'state/flowmux/team-usage'
        def observed(expected):
            for path in directory.glob('codex-*.json'):
                value = json.loads(path.read_text())
                if value.get('windows') and value['windows'][0]['used_percent'] == expected:
                    return value
            return None
        record = fixture.wait_for(lambda: observed(100), 'GUI collector publishes Codex usage', timeout=35)
        assert record['profile'] == str(h.root / 'codex'), record
        assert record['schema'] == 1 and 'tokens' not in record
        h.pass_check('Existing GUI usage collector publishes current profile-scoped limits without credentials')

        # Startup failures retry at 5, 20 and 60 seconds. Let them settle,
        # then cross the first 150-second periodic tick without a Team workspace.
        time.sleep(110)
        settled = observed(100)['collected_at']
        time.sleep(55)
        assert observed(100)['collected_at'] == settled
        h.pass_check('Disabled usage bar without a Team workspace does not keep polling after startup retries')

        workspace = h.rpc(socket, 'workspace_create', name='Team usage', root=str(h.root), team=True)['workspace_created']['id']
        source = h.workspace(socket, workspace)['panes'][0]['id']
        foreground = h.rpc(socket, 'workspace_create', name='Foreground', root=str(h.root))['workspace_created']['id']
        h.rpc(socket, 'workspace_focus', workspace=foreground)
        env = dict(h.env, FLOWMUX_SOCKET_PATH=str(socket), FLOWMUX_PANE_ID=source)
        def launch(*extra, expected=0):
            result = subprocess.run([sys.executable, str(recovery.HELPER), 'start', '--agent', 'auto',
                                     '--role', 'usage fixture', '--cwd', str(h.root), '--task-file', str(task),
                                     '--cli', args.cli, *extra], env=env, capture_output=True, text=True, timeout=25)
            assert result.returncode == expected, result.stdout + result.stderr
            return json.loads(result.stdout) if expected == 0 else result.stderr
        def finish(receipt):
            path = Path(receipt['job']) / 'status.json'
            state = fixture.wait_for(lambda: (s if (s := json.loads(path.read_text()))['state'] == 'completed' else None),
                                     'routed job completes', timeout=35)
            assert h.rpc(socket, 'workspace_current')['workspace_current']['id'] == foreground
            h.rpc(socket, 'pane_close', pane=state['pane'])
            return state
        receipt = launch()
        assert receipt['agent'] == 'claude', receipt
        assert receipt['routing_usage'][0]['status'] == 'exhausted'
        finish(receipt)
        assert not any(json.loads(path.read_text())['argv'][0].endswith('/codex') for path in h.root.glob('*-process.json'))
        before = len(h.workspace(socket, workspace)['panes'])
        assert 'no pane created' in launch('--agent', 'codex', '--no-fallback', expected=1)
        assert len(h.workspace(socket, workspace)['panes']) == before
        h.pass_check('Known exhausted Codex is skipped before any worker runs; no-fallback creates no pane')

        usage.write_text(json.dumps(dict(used=20, reset=int(time.time()) + 3600)))
        fixture.wait_for(lambda: observed(20), 'background Team refreshes usage with its bar disabled', timeout=185)
        assert not json.loads(options.read_text())['usage_bar_enabled']
        h.pass_check('Disabled usage bar still refreshes the existing collector for a background Team workspace')
        (h.root / 'behavior.json').write_text(json.dumps({'codex': 'quota'}))
        receipt = launch()
        assert receipt['agent'] == 'codex'
        usage.write_text(json.dumps(dict(error=True)))
        state = finish(receipt)
        assert state['agent'] == 'claude' and state['fallback_used'], state
        first_attempt = state['attempts'][0]
        assert first_attempt['blocker_kind'] == 'quota'
        receipt = launch()
        assert receipt['agent'] == 'claude' and receipt['routing_usage'][0]['reason'] == 'Recent native quota error', receipt
        finish(receipt)
        h.pass_check('A native quota failure is shared with the next job and avoids another exhausted Codex launch')

        quotas = h.root / 'state/flowmux/team-quota'
        usage.write_text(json.dumps(dict(used=30, reset=int(time.time()) + 3600)))
        fixture.wait_for(lambda: observed(30), 'fresh usage releases the old hold', timeout=185)
        (h.root / 'behavior.json').write_text('{}')
        receipt = launch()
        assert receipt['agent'] == 'codex' and receipt['routing_usage'][0]['status'] == 'available', receipt
        finish(receipt)
        assert not list(quotas.glob('codex-*.json')), 'released hold must not survive in the shared cache'
        usage.write_text(json.dumps(dict(error=True)))
        for path in directory.glob('codex-*.json'):
            saved = json.loads(path.read_text())
            saved['collected_at'] = time.time() - 400
            path.write_text(json.dumps(saved))
        receipt = launch()
        assert receipt['agent'] == 'codex', receipt
        finish(receipt)
        h.pass_check('A released quota hold stays released when its usage observation becomes stale')

        # A new native failure must still establish a fresh hold after release.
        (h.root / 'behavior.json').write_text(json.dumps({'codex': 'quota'}))
        state = finish(launch())
        assert state['agent'] == 'claude' and state['attempts'][0]['blocker_kind'] == 'quota', state
        assert list(quotas.glob('codex-*.json'))
        receipt = launch()
        assert receipt['agent'] == 'claude', receipt
        finish(receipt)
        h.pass_check('A new native quota error after release is shared with subsequent jobs')

        for path in quotas.glob('codex-*.json'):
            saved = json.loads(path.read_text())
            saved['until'] = time.time() - 1
            path.write_text(json.dumps(saved))
        (h.root / 'behavior.json').write_text('{}')
        receipt = launch()
        assert receipt['agent'] == 'codex', receipt
        finish(receipt)
        h.pass_check('After the quota hold expires, unknown usage allows a native attempt again')
    finally:
        h.cleanup()


if __name__ == '__main__':
    main()
