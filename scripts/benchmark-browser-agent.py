#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run BU Bench V1 through Flowmux and the configured Codex CLI authentication.

Raw tasks and traces stay in a private run directory. Codex judging reuses the
upstream V1 prompt, but is explicitly a replacement judge, not the Gemini result.
"""
import argparse
import base64
from concurrent.futures import ThreadPoolExecutor, as_completed
from datetime import datetime, timezone
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time


def write_json(path, value):
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    temporary.replace(path)


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def load_module(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def tasks(run):
    from cryptography.fernet import Fernet
    key = base64.urlsafe_b64encode(hashlib.sha256(b'BU_Bench_V1').digest())
    data = json.loads(Fernet(key).decrypt(base64.b64decode((run / 'BU_Bench_V1.enc').read_bytes())))
    if isinstance(data, dict):
        data = data['tasks']
    assert len(data) == len({t['task_id'] for t in data}) == 100
    return data


def clean_environment():
    return {k: v for k, v in os.environ.items()
            if not k.startswith('FLOWMUX_') and k not in ('CODEX_THREAD_ID', 'CODEX_SESSION_ID', 'CODEX_SURFACE_ID')}


def model_run(run, directory, prompt, name, timeout, schema, images=()):
    config = json.loads((run / 'config.json').read_text())
    schema_path = directory / f'{name}-schema.json'
    write_json(schema_path, schema)
    (directory / f'{name}-prompt.txt').write_text(prompt)
    command = ['rtk', 'proxy', config['codex'], '--no-daemon', '-a', 'never', 'exec',
               '--json', '--ephemeral', '--skip-git-repo-check', '--sandbox', 'danger-full-access',
               '-C', str(directory / 'workspace'), '-m', config['model'],
               '-c', f'model_reasoning_effort={json.dumps(config["reasoning"])}',
               '-c', 'web_search="disabled"',
               '--output-schema', str(schema_path), '-o', str(directory / f'{name}-answer.json')]
    for server in config['disabled_mcp_servers']:
        command.extend(['-c', f'mcp_servers.{server}.enabled=false'])
    for path in images:
        command.extend(['-i', str(path)])
    command.append('-')
    started = time.monotonic()
    with (directory / f'{name}-events.jsonl').open('w') as out, (directory / f'{name}-stderr.log').open('w') as err:
        process = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=out, stderr=err,
                                   text=True, env=clean_environment(), start_new_session=True)
        write_json(directory / f'{name}-process.json', {'pid': process.pid, 'command': command})
        timed_out = False
        try:
            process.communicate(prompt, timeout=timeout)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGTERM)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(process.pid, signal.SIGKILL)
                process.wait()
    usage, completed, violations, provider_messages = {}, False, [], []
    for line in (directory / f'{name}-events.jsonl').read_text().splitlines():
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get('type') == 'turn.completed':
            completed, usage = True, event.get('usage', {})
        if event.get('type') in ('error', 'turn.failed'):
            provider_messages.append(json.dumps(event))
        item = event.get('item', {})
        if item.get('type') in ('web_search', 'mcp_tool_call', 'collab_tool_call'):
            violations.append(item.get('type'))
    answer_path = directory / f'{name}-answer.json'
    answer = None
    if answer_path.exists():
        try:
            answer = json.loads(answer_path.read_text())
        except json.JSONDecodeError:
            pass
    provider_error = None
    errors = '\n'.join(provider_messages).lower()
    if process.returncode != 0 or not completed:
        for reason in ('usage_limit', 'rate_limit', 'quota', 'unauthorized', 'authentication', 'model_not_found'):
            if reason in errors:
                provider_error = reason
                break
    return {'exit_code': process.returncode, 'timed_out': timed_out,
            'turn_completed': completed, 'answer': answer, 'usage': usage,
            'provider_error': provider_error,
            'disallowed_tool_events': violations, 'seconds': time.monotonic() - started}


SOLVER_SCHEMA = {'type': 'object', 'additionalProperties': False,
                 'properties': {'completed': {'type': 'boolean'}, 'final_answer': {'type': 'string'},
                                'blocker': {'type': 'string'}},
                 'required': ['completed', 'final_answer', 'blocker']}
JUDGE_SCHEMA = {'type': 'object', 'additionalProperties': False,
                'properties': {'reasoning': {'type': 'string'}, 'verdict': {'type': 'boolean'},
                               'failure_reason': {'type': 'string'}, 'impossible_task': {'type': 'boolean'},
                               'reached_captcha': {'type': 'boolean'}},
                'required': ['reasoning', 'verdict', 'failure_reason', 'impossible_task', 'reached_captcha']}
JUDGE_TEXT_LIMIT = 40_000  # Per text section in the pinned BU V1 judge.
JUDGE_EVIDENCE_POLICY = 'complete-trajectory-v1'


def control(directory, arguments):
    """Serialize observations/actions so screenshot and reference histories cannot race."""
    with (directory / 'control.lock').open('w') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        connection = json.loads((directory / 'connection.json').read_text())
        log = directory / 'browser.jsonl'
        index = len(log.read_text().splitlines()) if log.exists() else 0
        if index >= connection['max_browser_calls']:
            raise SystemExit('Browser call budget exhausted; return the current result.')
        prefix = [connection['binary'], '--socket', connection['socket']]
        verb, *params = arguments
        if verb == 'tabs':
            command = [*prefix, '--json', 'tree']
        elif verb == 'switch':
            tree = json.loads(subprocess.check_output([*prefix, '--json', 'tree'], text=True))
            pane = next(p for w in tree['tree']['workspaces'] for p in w['panes'] if p['id'] == connection['pane'])
            if params[0] not in {t['id'] for t in pane['tabs'] if t.get('kind') == 'browser'}:
                raise SystemExit('Surface is not a browser tab in this task pane.')
            command = [*prefix, 'focus-tab', params[0], '--pane', connection['pane']]
        else:
            command = [*prefix, 'browser', '--surface', connection['surface'], verb, connection['pane'], *params]
        started = time.monotonic()
        try:
            result = subprocess.run(command, text=True, capture_output=True, timeout=40)
            row = {'step': index, 'arguments': arguments, 'exit_code': result.returncode,
                   'stdout': result.stdout, 'stderr': result.stderr}
        except subprocess.TimeoutExpired:
            row = {'step': index, 'arguments': arguments, 'exit_code': 124, 'stdout': '',
                   'stderr': 'CLI deadline exceeded; action outcome unknown. Observe before retrying.'}
        if verb == 'switch' and row['exit_code'] == 0:
            connection['surface'] = params[0]
            write_json(directory / 'connection.json', connection)
        row['seconds'] = time.monotonic() - started
        browser = [*prefix, 'browser', '--surface', connection['surface']]
        # Keep each post-command image attached to its command, including failures.
        shot = directory / 'screenshots' / f'{index:04d}.png'
        try:
            result = subprocess.run([*browser, 'screenshot', connection['pane'], str(shot)],
                                    text=True, capture_output=True, timeout=15)
            row['screenshot'] = str(shot) if result.returncode == 0 else None
            row['screenshot_error'] = result.stderr
        except subprocess.TimeoutExpired:
            row.update(screenshot=None, screenshot_error='Screenshot deadline exceeded')
        with log.open('a') as stream:
            stream.write(json.dumps(row, ensure_ascii=False) + '\n')
        print(row['stdout'], end='')
        print(row['stderr'], end='', file=sys.stderr)
        raise SystemExit(row['exit_code'])


def prepare_judge_evidence(rows, task, solver):
    """Compact without selecting evidence by outcome or silently dropping steps."""
    steps, seen, references = [], {}, 0
    for row in rows:
        output = row['stdout']
        if row['arguments'][:1] in (['snapshot'], ['find']):
            try:
                output = json.loads(output)
            except json.JSONDecodeError:
                pass
        key = json.dumps(output, ensure_ascii=False, separators=(',', ':'))
        step = {'step': row['step'], 'command': row['arguments'], 'exit_code': row['exit_code']}
        # A reference costs more than a short output such as true/false.
        if key in seen and len(key) > 40:
            step['output_same_as_step'] = seen[key]
            references += 1
        else:
            step['output'] = output
            seen.setdefault(key, row['step'])
        if row['stderr']:
            step['error'] = row['stderr']
        steps.append(json.dumps(step, ensure_ascii=False, separators=(',', ':')))
    answer = solver.get('answer') or {}
    if answer.get('blocker'):
        steps.append(json.dumps({'source': 'solver_self_report', 'blocker': answer['blocker']},
                                ensure_ascii=False, separators=(',', ':')))
    sizes = {'task': len(task['confirmed_task']), 'final_result': len(answer.get('final_answer', '')),
             'agent_trajectory': len('\n'.join(steps))}
    oversized = [f'{name}={size}' for name, size in sizes.items() if size > JUDGE_TEXT_LIMIT]
    evidence = {'policy': JUDGE_EVIDENCE_POLICY, 'source_steps': len(rows),
                'output_references': references, 'section_chars': sizes,
                'limit_per_section': JUDGE_TEXT_LIMIT,
                'review_required': ('Judge input exceeds the upstream character limit: ' + ', '.join(oversized))
                                   if oversized else None}
    return steps, evidence


def judge_task(run, directory, task, solver):
    rows = [json.loads(line) for line in (directory / 'browser.jsonl').read_text().splitlines()]
    steps, evidence = prepare_judge_evidence(rows, task, solver)
    write_json(directory / 'judge-evidence.json', evidence)
    result = {'exit_code': None, 'turn_completed': False, 'answer': None, 'usage': {},
              'disallowed_tool_events': [], 'evidence': evidence,
              'review_required': evidence['review_required']}
    if result['review_required']:
        return result
    config = json.loads((run / 'config.json').read_text())
    sys.path.insert(0, str(run / 'support'))
    from judge import construct_judge_messages
    images, seen = [], set()
    for row in reversed(rows):
        path = Path(row['screenshot']) if row.get('screenshot') else None
        if path and path.exists() and digest(path) not in seen:
            seen.add(digest(path))
            images.append(path)
        if len(images) == 10:
            break
    images.reverse()
    final = (solver.get('answer') or {}).get('final_answer', '')
    messages = construct_judge_messages(task=task['confirmed_task'], final_result=final,
                                       agent_steps=steps, ground_truth=task.get('answer'),
                                       screenshots_b64=[base64.b64encode(p.read_bytes()).decode() for p in images])
    prompt = messages[0].content + '\n\n' + messages[1].content[0].text
    sections = {'task': task['confirmed_task'], 'agent_trajectory': '\n'.join(steps), 'final_result': final}
    if task.get('answer'):
        sections['ground_truth'] = task['answer']
    if any(value and f'<{name}>\n{value}\n</{name}>' not in messages[1].content[0].text
           for name, value in sections.items()):
        result['review_required'] = evidence['review_required'] = 'Upstream judge altered or truncated evidence.'
        write_json(directory / 'judge-evidence.json', evidence)
        return result
    prompt += ('\nTrajectory JSON preserves all commands and outputs in order. Snapshot/find outputs may be'
               ' parsed JSON. output_same_as_step refers to the complete output at that earlier step;'
               ' omitted error fields mean empty stderr. solver_self_report is an unverified agent claim,'
               ' including its limitations, not independent evidence of success.')
    prompt += '\nUse only this evidence and the attached screenshots. Do not use tools. Return the requested JSON.'
    result.update(model_run(run, directory, prompt, 'judge', config['judge_timeout_seconds'], JUDGE_SCHEMA, images))
    return result


def run_task(run, task_id):
    config = json.loads((run / 'config.json').read_text())
    task = next(t for t in tasks(run) if t['task_id'] == task_id)
    directory = run / 'tasks' / task_id
    directory.mkdir(parents=True, exist_ok=False)
    (directory / 'workspace').mkdir()
    (directory / 'screenshots').mkdir()
    write_json(directory / 'state.json', {'state': 'starting', 'task_id': task_id})
    result = {'task_id': task_id, 'category': task['category'], 'score': None,
              'judge_kind': 'codex_replacement_with_upstream_v1_prompt', 'official_gemini_judged': False}
    try:
        bench = load_module('browser_bench', run / 'support/benchmark-browser-control.py')
        with bench.isolated_browser(run / 'bin/flowmux', directory) as (client, _, _, terminal):
            client.cli('close-pane', terminal)
            assert client.browser('wait', '--js', 'innerWidth >= 800', '--timeout-ms', '5000'), 'Pane layout did not settle'
            (directory / 'viewport.json').write_text(client.js(
                'JSON.stringify({width:innerWidth,height:innerHeight,devicePixelRatio})') + '\n')
            write_json(directory / 'connection.json', {'binary': str(client.binary), 'socket': str(client.socket),
                        'pane': client.pane, 'surface': client.surface, 'max_browser_calls': config['max_browser_calls']})
            client.browser('navigate', 'about:blank')
            assert client.browser('wait', '--condition', json.dumps({'UrlExact': 'about:blank'}))
            wrapper = f'rtk proxy {sys.executable} -B {run / "runner.py"} control --directory {directory} --'
            instructions = f'''You are the measured browser agent for one BU Bench V1 task.
Use ONLY Flowmux for all website observations and interactions. Do not use web search tools,
MCP, other agents, curl/HTTP clients, Playwright, browser-use, or other browser engines.
Do not read benchmark datasets, source code of challenge pages, rubrics, other tasks, or judge files.
Only read/write your workspace and your screenshots. Do not modify this runner or the browser.
You may use Python locally to create requested upload files or compute from observed information.
Do not perform real purchases, send messages to other people, or use personal accounts.
Public benchmark form submissions and fake test data are allowed.
The task has {config['task_timeout_seconds']} seconds and {config['max_browser_calls']} browser calls.
Work persistently, recover from transient errors, and verify outcomes. Report failures honestly.

Your browser starts at about:blank. Command syntax (no pane argument):
{wrapper} navigate URL
{wrapper} wait --url URL_PART --timeout-ms 15000
{wrapper} wait --ready-state complete --timeout-ms 15000
{wrapper} snapshot --compact --max-nodes 60 --max-chars 16000
{wrapper} find --role button --name NAME --exact --compact --max-chars 6000
{wrapper} click FULL_REF
{wrapper} fill FULL_REF TEXT
{wrapper} select FULL_REF VALUE
{wrapper} check FULL_REF
{wrapper} form '[{{"target":"FULL_REF","action":{{"fill":"VALUE"}}}}]'
{wrapper} press Enter
{wrapper} text FULL_REF
{wrapper} attr FULL_REF ATTRIBUTE
{wrapper} file-chooser status
{wrapper} file-chooser select ID ABSOLUTE_PATH --root {directory / 'workspace'}
{wrapper} dialog status
{wrapper} dialog accept ID
{wrapper} screenshot {directory / 'workspace' / 'view.png'}
{wrapper} tabs
{wrapper} switch SURFACE_UUID
For command help, use the wrapper COMMAND --help. Pointer and drag accept viewport coordinates.
You may use eval for bounded reads of rendered DOM, scrolling and focus; never inspect hidden
answers/scripts, inject success state, submit via internal APIs, or bypass a page challenge.
Wait returns true/false: check it. Every snapshot/find replaces refs; always use complete fresh refs.
Navigation requires a fresh snapshot. Prefer form for multiple fields. File chooser responds to a
pending file input. Every command is logged with a screenshot. View screenshot files when useful.
Do not confuse command success with task success. The final_answer field must contain exactly the
content and format requested by the task. If it asks for only an integer, name or short answer,
return only that answer in final_answer, with no explanation, citations or generic disclaimer.
Put limitations and anything not checked in blocker instead. Evidence is already retained in the
browser trace; add sources to final_answer only when the task requests them or allows elaboration.
completed is only your self-report and is never used directly as the benchmark score.

TASK:
{task['confirmed_task']}
'''
            (directory / 'workspace/AGENTS.md').write_text('# Benchmark workspace\nUse only the Flowmux wrapper supplied in the task prompt. Do not spawn agents.\n')
            write_json(directory / 'state.json', {'state': 'executing', 'task_id': task_id})
            result['solver'] = model_run(run, directory, instructions, 'solver', config['task_timeout_seconds'], SOLVER_SCHEMA)
            # The capture is recorded before closing the isolated browser.
            subprocess.run([sys.executable, '-B', str(run / 'runner.py'), 'control', '--directory', str(directory),
                            '--', 'snapshot', '--compact', '--max-nodes', '80', '--max-chars', '24000'],
                           capture_output=True, timeout=60)
        write_json(directory / 'execution.json', result)
        if result['solver'].get('provider_error'):
            write_json(run / 'provider-blocked.json', {'task_id': task_id, 'reason': result['solver']['provider_error']})
            raise RuntimeError('Executor provider unavailable; remaining tasks deferred.')
        write_json(directory / 'state.json', {'state': 'judging', 'task_id': task_id})
        result['judge'] = judge_task(run, directory, task, result['solver'])
        judge = result['judge']
        if judge.get('review_required'):
            result['review_required'] = judge['review_required']
        if judge.get('provider_error'):
            write_json(run / 'provider-blocked.json', {'task_id': task_id, 'reason': judge['provider_error']})
        if not judge.get('review_required') and judge['exit_code'] == 0 and judge['turn_completed'] and isinstance(judge['answer'], dict):
            import jsonschema
            jsonschema.validate(judge['answer'], JUDGE_SCHEMA)
            verdict = judge['answer'].get('verdict')
            if type(verdict) is bool:
                result['score'] = int(verdict)
        if result['solver']['disallowed_tool_events'] or judge['disallowed_tool_events']:
            result['score'] = None
            result['review_required'] = 'Disallowed external tools were called by executor or judge.'
    except Exception as error:
        result['harness_error'] = f'{type(error).__name__}: {error}'
    write_json(directory / 'result.json', result)
    write_json(directory / 'state.json', {'state': 'finished', 'task_id': task_id, 'score': result['score']})


def aggregate(run, selected):
    rows, usage = [], {}
    for task in selected:
        path = run / 'tasks' / task['task_id'] / 'result.json'
        if path.exists():
            result = json.loads(path.read_text())
            for role in ('solver', 'judge'):
                for key, count in result.get(role, {}).get('usage', {}).items():
                    if isinstance(count, int):
                        usage[key] = usage.get(key, 0) + count
            rows.append({'task_id': task['task_id'], 'category': task['category'], 'score': result['score'],
                         'harness_error': result.get('harness_error'),
                         'review_required': result.get('review_required'),
                         'judge_evidence_policy': result.get('judge', {}).get('evidence', {}).get('policy'),
                         'solver_timeout': result.get('solver', {}).get('timed_out'),
                         'judge_error': result.get('judge', {}).get('exit_code')})
    judged = sum(r['score'] is not None for r in rows)
    complete = len(rows) == judged == len(selected)
    categories = {}
    for task in selected:
        group = categories.setdefault(task['category'], {'selected': 0, 'finished': 0, 'judged': 0, 'passed': 0, 'failed': 0})
        group['selected'] += 1
    for row in rows:
        group = categories[row['category']]
        group['finished'] += 1
        group['judged'] += row['score'] is not None
        group['passed'] += row['score'] == 1
        group['failed'] += row['score'] == 0
    return {'updated_at': datetime.now(timezone.utc).isoformat(), 'selected': len(selected),
            'finished': len(rows), 'judged': judged, 'passed': sum(r['score'] == 1 for r in rows),
            'failed': sum(r['score'] == 0 for r in rows), 'unjudged_finished': len(rows) - judged,
            'success_rate_percent': 100 * sum(r['score'] == 1 for r in rows) / len(selected) if complete else None,
            'official_gemini_judged': False, 'judge_kind': 'codex_replacement_with_upstream_v1_prompt',
            'state': 'complete' if complete else 'running_or_incomplete', 'categories': categories,
            'reported_token_usage': usage, 'monetary_cost': None, 'tasks': rows}


def batch(run):
    config = json.loads((run / 'config.json').read_text())
    data = tasks(run)
    selected = [next(t for t in data if t['task_id'] == task_id) for task_id in config['task_ids']]
    write_json(run / 'status.json', aggregate(run, selected))
    def execute(task):
        task_id = task['task_id']
        if (run / 'provider-blocked.json').exists():
            print(json.dumps({'deferred': task_id, 'reason': 'provider unavailable'}), flush=True)
            return
        print(json.dumps({'started': task_id, 'category': task['category']}), flush=True)
        command = ['rtk', 'proxy', 'env', 'GDK_BACKEND=x11', 'XDG_CURRENT_DESKTOP=XFCE',
                   'xvfb-run', '-a', '-s', '-screen 0 1600x1100x24', 'dbus-run-session', '--',
                   sys.executable, '-B', str(run / 'runner.py'), 'task', '--run', str(run), '--task-id', task_id]
        with (run / f'{task_id}.log').open('w') as log:
            result = subprocess.run(command, stdout=log, stderr=log, env=clean_environment())
        if not (run / 'tasks' / task_id / 'result.json').exists():
            directory = run / 'tasks' / task_id
            directory.mkdir(parents=True, exist_ok=True)
            write_json(directory / 'result.json', {'task_id': task_id, 'category': task['category'],
                       'score': None, 'harness_error': f'Task process exited {result.returncode} without result'})
        print(json.dumps({'finished': task_id, 'exit_code': result.returncode}), flush=True)
    with ThreadPoolExecutor(max_workers=config['parallel']) as pool:
        futures = [pool.submit(execute, task) for task in selected]
        for future in as_completed(futures):
            future.result()
            write_json(run / 'status.json', aggregate(run, selected))
    final = aggregate(run, selected)
    final['state'] = 'complete' if final['judged'] == len(selected) else 'finished_with_unjudged_tasks'
    if (run / 'provider-blocked.json').exists():
        final['state'] = 'blocked_on_provider'
    write_json(run / 'status.json', final)


def self_test():
    import tempfile
    from types import SimpleNamespace
    from unittest.mock import Mock, patch

    task = {'confirmed_task': 'Read the page.', 'answer': 'A synthetic reference answer.'}
    solver = {'answer': {'final_answer': 'FINAL_ANSWER', 'blocker': 'Pagination was not verified.'}}
    snapshot = {'page': {'text': '관찰 내용 ' * 4000, 'url': 'https://example.test/'},
                'refs': {'e1': {'name': 'Submit', 'states': {'disabled': True}}}, 'truncated': True}
    rows = [{'step': index, 'arguments': [verb], 'exit_code': code, 'stdout': output, 'stderr': error}
            for index, (verb, code, output, error) in enumerate([
                ('snapshot', 0, json.dumps(snapshot, ensure_ascii=False, indent=2), ''),
                ('snapshot', 0, json.dumps(snapshot, ensure_ascii=False), ''),
                ('eval', 0, 'MIDDLE_EXTRACTION', ''),
                ('click', 1, '', 'ACTION_FAILED'),
                ('snapshot', 0, '{"page":{"text":"FINAL_STATE"},"truncated":false}', '')])]
    assert sum(len(row['stdout']) for row in rows) > JUDGE_TEXT_LIMIT
    steps, evidence = prepare_judge_evidence(rows, task, solver)
    assert not evidence['review_required']
    assert evidence['section_chars']['agent_trajectory'] == len('\n'.join(steps)) <= JUDGE_TEXT_LIMIT
    packed = [json.loads(step) for step in steps]
    assert packed[1]['output_same_as_step'] == 0
    for original, compacted in zip(rows, packed):
        output = (packed[compacted['output_same_as_step']]['output']
                  if 'output_same_as_step' in compacted else compacted['output'])
        expected = json.loads(original['stdout']) if original['arguments'] == ['snapshot'] else original['stdout']
        assert output == expected
        assert compacted['command'] == original['arguments']
        assert compacted['exit_code'] == original['exit_code']
        assert compacted.get('error', '') == original['stderr']
    assert packed[-1] == {'source': 'solver_self_report', 'blocker': solver['answer']['blocker']}

    # Every bounded section accepts exactly 40k, defers at 40k + 1.
    for section in ('task', 'final_result', 'agent_trajectory'):
        boundary_task, boundary_solver = {'confirmed_task': ''}, {'answer': {'final_answer': ''}}
        boundary_rows = [dict(rows[2], stdout='')]
        overhead = prepare_judge_evidence(boundary_rows, boundary_task, boundary_solver)[1]['section_chars']['agent_trajectory']
        for extra in (0, 1):
            if section == 'task':
                boundary_task['confirmed_task'] = 'x' * (JUDGE_TEXT_LIMIT + extra)
            elif section == 'final_result':
                boundary_solver['answer']['final_answer'] = 'x' * (JUDGE_TEXT_LIMIT + extra)
            else:
                boundary_rows[0]['stdout'] = 'x' * (JUDGE_TEXT_LIMIT - overhead + extra)
            report = prepare_judge_evidence(boundary_rows, boundary_task, boundary_solver)[1]
            assert report['section_chars'][section] == JUDGE_TEXT_LIMIT + extra
            assert bool(report['review_required']) == bool(extra)

    with tempfile.TemporaryDirectory() as temporary:
        root = Path(temporary)
        write_json(root / 'config.json', {'judge_timeout_seconds': 1})
        log = root / 'browser.jsonl'
        log.write_text('\n'.join(json.dumps(row) for row in rows) + '\n')

        def construct(**kwargs):
            sections = {'task': kwargs['task'], 'agent_trajectory': '\n'.join(kwargs['agent_steps']),
                        'final_result': kwargs['final_result'], 'ground_truth': kwargs['ground_truth']}
            content = '\n'.join(f'<{name}>\n{value}\n</{name}>' for name, value in sections.items())
            return [SimpleNamespace(content='Judge.'), SimpleNamespace(content=[SimpleNamespace(text=content)])]

        model = Mock(return_value={'exit_code': 0, 'turn_completed': True, 'answer': {'verdict': True}})
        constructor = Mock(side_effect=construct)
        with patch.dict(sys.modules, {'judge': SimpleNamespace(construct_judge_messages=constructor)}), \
                patch.dict(globals(), {'model_run': model}), patch.object(sys, 'path', list(sys.path)):
            assert judge_task(root, root, task, solver)['exit_code'] == 0
            prompt = model.call_args.args[2]
            assert all(marker in prompt for marker in ('MIDDLE_EXTRACTION', 'ACTION_FAILED', 'FINAL_STATE', 'FINAL_ANSWER'))
            assert solver['answer']['blocker'] in prompt
            model.reset_mock()
            constructor.reset_mock()
            log.write_text(json.dumps(dict(rows[2], stdout='x' * JUDGE_TEXT_LIMIT)) + '\n')
            deferred = judge_task(root, root, task, solver)
            assert deferred['review_required'] and deferred['answer'] is None
            model.assert_not_called()
            constructor.assert_not_called()
            assert json.loads((root / 'judge-evidence.json').read_text())['review_required']
            log.write_text('\n'.join(json.dumps(row) for row in rows) + '\n')
            constructor.side_effect = lambda **kwargs: [SimpleNamespace(content='Judge.'),
                                                        SimpleNamespace(content=[SimpleNamespace(text='truncated')])]
            assert judge_task(root, root, task, solver)['review_required']
            model.assert_not_called()

        selected = [{'task_id': 'one', 'category': 'test'}, {'task_id': 'two', 'category': 'test'}]
        assert aggregate(root, selected)['success_rate_percent'] is None
        for task in selected:
            (root / 'tasks' / task['task_id']).mkdir(parents=True)
        write_json(root / 'tasks/one/result.json', {'score': 1})
        assert aggregate(root, selected)['success_rate_percent'] is None
        write_json(root / 'tasks/two/result.json', {'score': None, 'judge': deferred,
                                                  'review_required': deferred['review_required']})
        assert aggregate(root, selected)['unjudged_finished'] == 1
        assert aggregate(root, selected)['success_rate_percent'] is None
        assert aggregate(root, selected)['failed'] == 0
        assert aggregate(root, selected)['tasks'][1]['review_required']
        write_json(root / 'tasks/two/result.json', {'score': 0})
        assert aggregate(root, selected)['success_rate_percent'] == 50
    print('BROWSER_AGENT_SELF_TEST_OK')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['batch', 'task', 'control', 'self-test'])
    parser.add_argument('--run', type=Path)
    parser.add_argument('--directory', type=Path)
    parser.add_argument('--task-id')
    args, remaining = parser.parse_known_args()
    if args.mode == 'self-test':
        self_test()
    elif args.mode == 'control':
        control(args.directory, remaining[1:] if remaining[:1] == ['--'] else remaining)
    elif args.mode == 'task':
        run_task(args.run, args.task_id)
    else:
        batch(args.run)


if __name__ == '__main__':
    main()
