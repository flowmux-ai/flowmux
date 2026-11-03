#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify native Claude prompt-hook activation in isolated real Flowmux PTYs.

Uses the signed-in Claude account and real worker CLIs. User settings, hook trust,
and active sessions stay untouched. Requires Xvfb and python3-xlib.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import sys
import uuid

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('fixture', REPO / 'scripts/test-ssh-workspace-gui.py')
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


def verify_workers(jobs, workspace, source_pane, tree, previous_workers=None):
    owned = {}
    for path in jobs.glob('job-*/job.json'):
        job = json.loads(path.read_text())
        if job['workspace'] == workspace:
            owned[path.parent] = job
    assert owned and len(tree['panes']) > 1, 'Team lead did not create a visible worker'
    assert all(job['source_pane'] == source_pane for job in owned.values()), 'Worker recursively delegated'
    workers = {(job['pane'], job['surface']) for job in owned.values()}
    for path in owned:
        status = json.loads((path / 'status.json').read_text())
        assert status['state'] == status.get('task_status') == 'completed', status
        assert (path / 'result.md').read_text().strip(), 'No collected worker report'
        for turn in path.glob('turns/*/status.json'):
            followup = json.loads(turn.read_text())
            assert followup['state'] == followup.get('task_status') == 'completed', followup
            assert followup['session_id'] == status['session_id'], 'Worker conversation changed'
    if previous_workers is not None:
        assert workers == previous_workers, 'Related followup opened another worker'
        assert any(path.read_text().strip() for path in jobs.glob('job-*/turns/*/result.md')), 'No completed worker followup'
    return workers


def has_context_result(records, mode, pane):
    for record in records:
        for block in record.get('message', {}).get('content', []):
            if not isinstance(block, dict) or block.get('type') != 'tool_result':
                continue
            try:
                value, _ = json.JSONDecoder().raw_decode(block.get('content', '').lstrip())
            except (AttributeError, ValueError):
                continue
            if isinstance(value, dict) and value.get('mode') == mode and value.get('pane') == pane:
                return True
    return False


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default=str(REPO / 'target/debug/flowmux'))
    parser.add_argument('--cli', default=str(REPO / 'target/debug/flowmuxctl'))
    parser.add_argument('--timeout', type=int, default=600)
    parser.add_argument('--protected-pid', type=int, action='append', default=[])
    args = parser.parse_args()
    args.gui, args.cli = str(Path(args.gui).resolve()), str(Path(args.cli).resolve())
    executable = shutil.which('claude')
    assert executable, 'Claude must be installed and signed in'
    h = fixture.Harness(args)
    for key in ('CODEX_THREAD_ID', 'CODEX_SESSION_ID', 'CLAUDECODE'):
        h.env.pop(key, None)
    # Do not let this isolated GUI refresh the user's installed skill files.
    marker = h.root / 'state/flowmux/installed-skill-version'
    marker.parent.mkdir(parents=True)
    marker.write_text('9999.0.0')
    profile = h.root / 'claude'
    profile.mkdir()
    original = Path(os.environ.get('CLAUDE_CONFIG_DIR') or Path.home() / '.claude')
    for name in ('.credentials.json', '.claude.json'):
        if (original / name).is_file():
            if name == '.credentials.json':
                (profile / name).symlink_to(original / name)
            else:
                shutil.copyfile(original / name, profile / name)
    h.env['CLAUDE_CONFIG_DIR'] = str(profile)
    h.env['FLOWMUX_BUNDLED_CLI_PATH'] = args.cli
    subprocess.run([args.cli, 'agent', 'install', '--agent', 'claude-code', '--skill', 'flowmux-team'],
                   env=h.env, check=True, capture_output=True)
    hooks = {}
    for event, command in [('SessionStart', 'session-start'), ('UserPromptSubmit', 'prompt-submit'),
                           ('PreToolUse', 'pre-tool-use'), ('PostToolUse', 'post-tool-use'),
                           ('Stop', 'stop'), ('SessionEnd', 'session-end')]:
        hooks[event] = [dict(hooks=[dict(type='command', command=shlex.join(
            [args.cli, 'hooks', 'claude', command]), timeout=5)])]
    settings = profile / 'settings.json'
    settings.write_text(json.dumps(dict(hooks=hooks)))
    options = h.root / 'config/flowmux/options.json'
    options.parent.mkdir(parents=True)
    options.write_text(json.dumps(dict(default_shell='/bin/sh', auto_resume_agent_sessions=False)))
    project = h.root / 'project'
    project.mkdir()
    (project / 'paths.py').write_text(
        'from pathlib import Path\n\n'
        'def allowed_path(root, name):\n'
        '    """Return a path only if it is inside root."""\n'
        '    candidate = (Path(root) / name).resolve()\n'
        '    return candidate if str(candidate).startswith(str(Path(root).resolve())) else None\n')
    (project / 'cache.py').write_text(
        'class Cache:\n'
        '    """Cache values for ttl seconds; missing values are represented by None."""\n'
        '    def __init__(self, ttl):\n'
        '        self.ttl, self.items = ttl, {}\n'
        '    def set(self, key, value, now):\n'
        '        self.items[key] = (value, now + self.ttl)\n'
        '    def get(self, key, now):\n'
        '        value, expires = self.items.get(key, (None, 0))\n'
        '        return value if now <= expires else None\n')
    prompt = (f'Review {project / "paths.py"} for containment defects and {project / "cache.py"} for expiration-boundary defects. '
              'Investigate these two areas independently and combine evidence-backed findings. '
              'Obtain an independent review of the findings before finalizing. '
              f'If delegating, use Claude workers with launch cwd {REPO}, the existing trusted checkout, '
              f'and environment CLAUDE_CONFIG_DIR={shlex.quote(str(original))} to retain the existing provider trust decisions. '
              'Review only the absolute fixture paths above. '
              'Do not edit the source files, commit, or push. Report paths and relevant lines. '
              'Use bounded checks; report blockers honestly.')
    try:
        h.start_display()
        process, socket = h.window('activation')
        for team in (False, True):
            mode = 'team' if team else 'inline'
            workspace = h.rpc(socket, 'workspace_create', name=f'Activation {mode}', root=str(project),
                              team=team)['workspace_created']['id']
            pane = h.workspace(socket, workspace)['panes'][0]['id']
            session = str(uuid.uuid4())
            turns = [('review', prompt)]
            if team:
                turns.append(('continue', 'Have your previous reviewer recheck the findings against the same files, then give a concise final review. Do not edit.'))
            for index, (name, task) in enumerate(turns):
                log = h.root / f'{mode}-{name}.jsonl'
                done = h.root / f'{mode}-{name}.exit'
                command = [executable, '--print', '--verbose', '--output-format', 'stream-json',
                           '--include-hook-events', '--model', 'sonnet', '--effort', 'medium',
                           '--allow-dangerously-skip-permissions', '--permission-mode', 'bypassPermissions',
                           '--strict-mcp-config', '--mcp-config', '{"mcpServers":{}}',
                           '--resume' if index else '--session-id', session, task]
                h.send(socket, pane, shlex.join(command) + ' >' + shlex.quote(str(log))
                       + ' 2>&1; echo $? >' + shlex.quote(str(done)))
                fixture.wait_for(lambda: done.exists(), f'{mode} {name} native lead completes', args.timeout)
                assert done.read_text().strip() == '0', log.read_text()[-5000:]
                records = [json.loads(line) for line in log.read_text().splitlines() if line.startswith('{')]
                result = next(record for record in reversed(records) if record.get('type') == 'result')
                assert not result.get('is_error'), result
                assert result.get('session_id') == session, 'Lead conversation changed'
                # Native hook event receipts prove context came from the configured hook.
                assert any('Flowmux workspace execution:' in json.dumps(record) for record in records
                           if record.get('type') == 'system'), 'Native hook context receipt missing'
                assert has_context_result(records, mode, pane), 'Lead did not resolve its actual workspace'
                tree = h.workspace(socket, workspace)
                (h.root / f'{mode}-{name}-tree.json').write_text(json.dumps(tree, indent=2))
                jobs = h.root / 'state/flowmux/team-jobs'
                manifests = [json.loads(path.read_text()) for path in jobs.glob('job-*/job.json')]
                owned = [job for job in manifests if job['workspace'] == workspace]
                if team:
                    previous_workers = verify_workers(jobs, workspace, pane, tree,
                                                      previous_workers if index else None)
                else:
                    assert not owned and len(tree['panes']) == 1, 'Ordinary workspace created a worker'
                h.pass_check(f'{mode}/{name}: native hook context, workspace discovery and expected pane behavior')
            for worker in h.workspace(socket, workspace)['panes']:
                (h.root / f'{mode}-{worker["id"]}.screen.txt').write_text(h.screen(socket, worker['id']))
        h.assert_protected()
        h.stop(process)
    finally:
        h.cleanup()
        for name in ('.credentials.json', '.claude.json'):
            (profile / name).unlink(missing_ok=True)


if __name__ == '__main__':
    main()
