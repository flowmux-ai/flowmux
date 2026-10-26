#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Measure real Flowmux CLI workflows; retain failed runs and verify final state."""
import argparse
from concurrent.futures import ThreadPoolExecutor
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
import threading
import time


def percentile(values, fraction):
    return sorted(values)[max(0, math.ceil(len(values) * fraction) - 1)]


def summarize(rows):
    measured = [row for row in rows if row['phase'] == 'measurement']
    passed = [row for row in measured if row['success']]
    result = {'attempted': len(measured), 'passed': len(passed),
              'failed': len(measured) - len(passed),
              'warmup_failed': sum(not row['success'] for row in rows if row['phase'] == 'warmup')}
    if passed:
        values = [row['elapsed_ms'] for row in passed]
        result.update(p50_ms=statistics.median(values), min_ms=min(values), max_ms=max(values),
                      mean_calls=statistics.mean(row['calls'] for row in passed),
                      mean_response_bytes=statistics.mean(row['response_bytes'] for row in passed))
        if len(passed) >= 100:
            result['p95_ms'] = percentile(values, .95)
    return result


class Browser:
    def __init__(self, binary, socket, env, output):
        self.binary, self.socket, self.env, self.output = binary, socket, env, output
        self.pane = None
        self.surface = None
        self.calls = []
        self.rows = []
        self.lock = threading.Lock()
        self.stage = 'setup'

    def cli(self, *args, error=False, pane_context=None):
        env = self.env | ({'FLOWMUX_PANE_ID': pane_context} if pane_context else {})
        start = time.perf_counter_ns()
        result = subprocess.run([str(self.binary), '--socket', str(self.socket), '--json', *map(str, args)],
                                env=env, capture_output=True, timeout=35)
        row = {'stage': self.stage, 'args': list(map(str, args)),
               'elapsed_ms': (time.perf_counter_ns() - start) / 1e6,
               'response_bytes': len(result.stdout), 'exit_code': result.returncode,
               'expected_error': error}
        with self.lock:
            self.calls.append(row)
            with (self.output / 'commands.jsonl').open('a') as stream:
                stream.write(json.dumps(row, ensure_ascii=False) + '\n')
        assert bool(result.returncode) == error, (args, result.stdout.decode()[:2000], result.stderr.decode()[:2000])
        response = json.loads(result.stdout) if result.stdout else {'error': result.stderr.decode()}
        if error:
            assert 'error' in response, response
        return response

    def browser(self, op, *args, surface=None, error=False):
        response = self.cli('browser', '--surface', surface or self.surface, op, self.pane, *args, error=error)
        if error:
            return response
        if 'browser_result' in response:
            return response['browser_result']['value']
        if 'browser_bool_result' in response:
            return response['browser_bool_result']['value']
        return response

    def js(self, source):
        return self.browser('eval', source)

    def snapshot(self, *args, surface=None):
        return json.loads(self.browser('snapshot', *args, surface=surface))

    def refs(self):
        return {value['name']: key for key, value in self.snapshot('--compact')['refs'].items()}

    def navigate(self, url):
        # A ready document at the same URL can belong to the previous trial.
        if self.browser('url') == url:
            self.browser('navigate', 'about:blank')
            assert self.browser('wait', '--condition', json.dumps({'UrlExact': 'about:blank'}))
        self.browser('navigate', url)
        assert self.browser('wait', '--condition', json.dumps({'UrlExact': url}), '--timeout-ms', '20000')
        assert self.browser('wait', '--ready-state', 'complete', '--timeout-ms', '20000')

    def measure(self, name, prepare, action, samples, warmup=5):
        rows = []
        failures = 0
        for index in range(warmup + samples):
            phase = 'warmup' if index < warmup else 'measurement'
            self.stage = f'{name}/{phase}/{index}/setup'
            row = {'case': name, 'phase': phase, 'index': index, 'success': False}
            start = time.perf_counter_ns()
            first = len(self.calls)
            try:
                prepared = prepare(index)
                self.stage = f'{name}/{phase}/{index}/measured'
                first = len(self.calls)
                start = time.perf_counter_ns()
                evidence = action(prepared, index)
                row.update(success=True, evidence=evidence)
                failures = 0
            except Exception as error:
                row.update(error=f'{type(error).__name__}: {error}', failed_stage=self.stage)
                failures += 1
            row.update(elapsed_ms=(time.perf_counter_ns() - start) / 1e6,
                       calls=len(self.calls) - first,
                       response_bytes=sum(call['response_bytes'] for call in self.calls[first:]))
            rows.append(row)
            self.rows.append(row)
            with (self.output / 'samples.jsonl').open('a') as stream:
                stream.write(json.dumps(row, ensure_ascii=False) + '\n')
            if failures >= 3:
                print(f'CASE_ABORT {name}: three consecutive failures; {row.get("error")}', flush=True)
                break
        self.stage = 'setup'
        summary = summarize(rows)
        summary.update(requested_samples=samples, not_run=samples - summary['attempted'])
        print(json.dumps({'case': name, **summary}, ensure_ascii=False), flush=True)
        return summary


@contextmanager
def isolated_browser(binary, output):
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location('browser_fixture', Path(__file__).with_name('test-browser-control.py'))
    suite = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(suite)
    with tempfile.TemporaryDirectory(prefix='fm-bench-', dir='/tmp') as directory:
        root = Path(directory)
        env = {key: value for key, value in os.environ.items() if not key.startswith('FLOWMUX_')}
        for key, child in [('FLOWMUX_RUNTIME_DIR', 'run'), ('XDG_RUNTIME_DIR', 'run'),
                           ('XDG_CONFIG_HOME', 'config'), ('XDG_DATA_HOME', 'data'),
                           ('XDG_STATE_HOME', 'state'), ('XDG_CACHE_HOME', 'cache')]:
            (root / child).mkdir(mode=0o700, exist_ok=True)
            env[key] = str(root / child)
        marker = root / 'state/flowmux/welcome-seen'
        marker.parent.mkdir(parents=True)
        marker.touch()
        env.update(SHELL='/bin/sh', GSK_RENDERER='cairo', FLOWMUX_BUNDLED_CLI_PATH=str(binary.with_name('flowmuxctl')))
        server = suite.ThreadingHTTPServer(('127.0.0.1', 0), suite.Fixture)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        with (output / 'gui.log').open('w') as log:
            process = subprocess.Popen([str(binary)], cwd=root, env=env, stdout=log, stderr=log)
            try:
                socket = root / 'run' / f'flowmux-{process.pid}.sock'
                deadline = time.monotonic() + 30
                while not socket.exists():
                    assert process.poll() is None, 'GUI exited during startup'
                    assert time.monotonic() < deadline, 'GUI startup timeout'
                    time.sleep(.05)
                client = Browser(binary.with_name('flowmuxctl'), socket, env, output)
                assert client.cli('ping') == 'pong'
                workspace = client.cli('workspace', 'new', '--name', 'Browser evaluation', '--root', str(root))['workspace_created']['id']
                terminal = next(item for item in client.cli('tree')['tree']['workspaces'] if item['id'] == workspace)['panes'][0]['id']
                url = f'http://127.0.0.1:{server.server_port}/'
                client.pane = client.cli('browser', 'open', url, pane_context=terminal)['browser_pane_opened']['pane']
                assert client.cli('browser', 'wait', client.pane, '--url', url)['browser_bool_result']['value']
                assert client.cli('browser', 'wait', client.pane, '--ready-state', 'complete')['browser_bool_result']['value']
                snapshot = json.loads(client.cli('browser', 'snapshot', client.pane, '--compact')['browser_result']['value'])
                client.surface = snapshot['page']['surface_id']
                (output / 'diagnostics.json').write_text(client.browser('diagnose') + '\n')
                yield client, url, root, terminal
            finally:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                server.shutdown()
                server.server_close()
                thread.join()


def local_cases(client, url, root, terminal, samples, warmup):
    summaries = {}

    def case(name, prepare, action, count=samples):
        summaries[name] = client.measure(name, prepare, action, count, warmup)

    def observe(options, expected):
        snap = (json.loads(client.browser('find', '--role', 'button', '--name', 'Target 0', '--exact', '--compact'))
                if options == 'find' else client.snapshot(*options))
        assert any(meta['name'] == 'Target 0' for meta in snap['refs'].values()), 'target missing'
        if expected is not None:
            assert len(snap['refs']) == expected, (len(snap['refs']), snap['truncated'])
        return {'refs': len(snap['refs']), 'truncated': snap['truncated']}

    for size in (20, 500):
        client.js(f"document.body.innerHTML=Array.from({{length:{size}}},(_,i)=>'<button>Target '+i+'</button><p>Row '+i+' benchmark text</p>').join('');'ready'")
        for name, options in [('full', []), ('compact', ['--compact']), ('find', 'find')]:
            case(f'observe_{size}_{name}', lambda i: None,
                 lambda _, i, options=options, size=size: observe(options, 1 if options == 'find' else size))

    client.js("document.body.innerHTML='<button id=go>Run</button><p id=result role=status>0</p>'+Array.from({length:5},(_,i)=>'<label>Field '+i+'<input id=f'+i+'></label>').join('')+'<div role=textbox aria-label=Rich contenteditable id=rich></div>';window.clicks=0;go.onclick=e=>{window.trusted=e.isTrusted;result.textContent=String(++clicks)};f0.oninput=e=>window.inputTrusted=e.isTrusted;'ready'")

    def prepare_click(_):
        client.js("window.clicks=0;result.textContent='0';'reset'")
        return client.refs()['Run']

    def click(ref, _, receipt=False):
        if receipt:
            result = json.loads(client.browser('click', ref, '--receipt', '--expect-text', '1'))
            assert result['status'] == 'outcome_confirmed', result
        else:
            client.browser('click', ref)
        assert client.js('JSON.stringify([clicks,window.trusted])') == '[1,true]'
        return {'clicks': 1, 'trusted': True}

    case('native_click_verify', prepare_click, click)
    case('native_click_receipt_verify', prepare_click, lambda r, i: click(r, i, True))

    def prepare_field(_):
        client.js("f0.value='';window.inputTrusted=false;'reset'")
        ref = client.refs()['Field 0']
        client.browser('focus', ref)
        return ref

    def fill(ref, index, native=False):
        text = f'한글😀-{index}'
        client.browser('type', text) if native else client.browser('fill', ref, text)
        assert client.browser('value', ref) == text
        if native:
            assert client.js('String(window.inputTrusted)') == 'true'
        return {'value': text, 'native': native}

    case('fill_verify', prepare_field, fill)
    case('native_type_verify', prepare_field, lambda ref, i: fill(ref, i, True))

    def prepare_key(index):
        ref = prepare_field(index)
        client.browser('fill', ref, 'benchmark')
        return ref

    def key(_, index):
        client.browser('press', 'Control+a')
        assert client.js("String(f0.selectionStart)+':'+f0.selectionEnd") == '0:9'
        return {'selection': [0, 9]}

    case('native_press_verify', prepare_key, key)

    def prepare_form(_):
        client.js("for(let i=0;i<5;i++)document.getElementById('f'+i).value='';'reset'")
        refs = client.refs()
        return [refs[f'Field {i}'] for i in range(5)]

    def form(refs, index, batch=False):
        values = [f'value-{index}-{i}' for i in range(5)]
        if batch:
            result = json.loads(client.browser('form', json.dumps([{'target': ref, 'action': {'fill': value}} for ref, value in zip(refs, values)])))
            assert [item['status'] for item in result['fields']] == ['verified'] * 5, result
        else:
            for ref, value in zip(refs, values):
                client.browser('fill', ref, value)
        assert json.loads(client.js("JSON.stringify(Array.from({length:5},(_,i)=>document.getElementById('f'+i).value))")) == values
        return {'fields_verified': 5}

    case('form_individual_5_verify', prepare_form, form)
    case('form_batch_5_verify', prepare_form, lambda refs, i: form(refs, i, True))

    def prepare_wait(_):
        client.js("result.textContent='pending';setTimeout(()=>result.textContent='finished',250);'scheduled'")

    def wait(_, index, polling=False):
        if polling:
            deadline = time.monotonic() + 5
            while 'finished' not in json.dumps(client.snapshot('--compact')):
                assert time.monotonic() < deadline, 'snapshot polling timeout'
                time.sleep(.025)
        else:
            assert client.browser('wait', '--text', 'finished', '--poll-ms', '25', '--timeout-ms', '5000')
        assert client.js('result.textContent') == 'finished'
        return {'injected_delay_ms': 250}

    case('wait_typed_250ms', prepare_wait, wait)
    case('wait_snapshot_poll_250ms', prepare_wait, lambda p, i: wait(p, i, True))

    def prepare_stale(_):
        client.js("f0.value='';'reset'")
        ref = client.refs()['Field 0']
        client.js("f0.replaceWith(f0.cloneNode());'replaced'")
        return ref

    def stale(ref, _):
        result = client.browser('fill', ref, 'wrong', error=True)
        assert client.js('f0.value') == ''
        return {'rejected': True, 'wrong_target_writes': 0, 'error': str(result)[:400]}

    case('stale_ref_rejection_verify', prepare_stale, stale)
    client.navigate(url)
    assert client.browser('wait', '--js', "!!document.querySelector('#frame')?.contentDocument?.querySelector('input')")
    for field in ('Frame field', 'Shadow field'):
        def nested(ref, index):
            value = f'nested-{index}'
            client.browser('fill', ref, value)
            assert client.browser('value', ref) == value
            return {'value': value}
        case(field.lower().replace(' ', '_') + '_verify', lambda i, field=field: client.refs()[field], nested)

    surfaces = [client.surface]
    for _ in range(3):
        pane = client.cli('browser', 'open', url, pane_context=terminal)['browser_pane_opened']['pane']
        assert pane == client.pane
        assert client.cli('browser', 'wait', pane, '--ready-state', 'complete')['browser_bool_result']['value']
        snap = json.loads(client.cli('browser', 'snapshot', pane, '--compact')['browser_result']['value'])
        surfaces.append(snap['page']['surface_id'])
    for count in (1, 2, 4):
        def concurrent(_, index, count=count):
            with ThreadPoolExecutor(max_workers=count) as pool:
                snaps = list(pool.map(lambda surface: client.snapshot('--compact', surface=surface), surfaces[:count]))
            assert [snap['page']['surface_id'] for snap in snaps] == surfaces[:count]
            assert all(any(ref['name'] == 'Field' for ref in snap['refs'].values()) for snap in snaps)
            return {'completed_observations': count}
        case(f'concurrent_observe_{count}', lambda i: None, concurrent)
    return summaries


def public_cases(client, runs):
    base = 'https://the-internet.herokuapp.com'
    summaries = {}

    def login_prepare(_):
        client.navigate(base + '/login')
        return client.refs()

    def login(refs, _):
        client.browser('fill', refs['Username'], 'tomsmith')
        client.browser('fill', refs['Password'], 'SuperSecretPassword!')
        client.browser('click', refs['Login'])
        assert client.browser('wait', '--condition', json.dumps({'UrlExact': base + '/secure'}), '--timeout-ms', '20000')
        assert client.browser('wait', '--text', 'You logged into a secure area!')
        return {'authenticated': True}

    summaries['public_login'] = client.measure('public_login', login_prepare, login, runs, 0)

    def dynamics(refs, _):
        client.browser('click', refs['Remove'])
        assert client.browser('wait', '--js', "!document.querySelector('#checkbox')", '--timeout-ms', '15000')
        refs = client.refs()
        client.browser('click', refs['Enable'])
        assert client.browser('wait', '--js', "!document.querySelector('#input-example input').disabled", '--timeout-ms', '15000')
        snap = client.snapshot('--compact')
        field = next(ref for ref, meta in snap['refs'].items() if meta['role'] == 'textbox')
        client.browser('fill', field, 'benchmark-ready')
        assert client.browser('value', field) == 'benchmark-ready'
        return {'checkbox_removed': True, 'input_value': 'benchmark-ready'}

    def load(path):
        client.navigate(base + path)
        return client.refs()

    summaries['public_dynamic_controls'] = client.measure('public_dynamic_controls', lambda i: load('/dynamic_controls'), dynamics, runs, 0)

    def add_remove(refs, _):
        for _ in range(3):
            client.browser('click', refs['Add Element'])
        for _ in range(2):
            client.browser('click', client.refs()['Delete'])
        assert client.js("String(document.querySelectorAll('#elements button').length)") == '1'
        return {'remaining_buttons': 1}

    summaries['public_add_remove'] = client.measure('public_add_remove', lambda i: load('/add_remove_elements/'), add_remove, runs, 0)

    def table(_, index):
        result = json.loads(client.js("JSON.stringify([...document.querySelectorAll('#table1 tbody tr')].map(r=>[...r.cells].map(c=>c.textContent.trim())).sort((a,b)=>Number(b[3].slice(1))-Number(a[3].slice(1)))[0].slice(0,4))"))
        assert result == ['Doe', 'Jason', 'jdoe@hotmail.com', '$100.00'], result
        return {'answer': result, 'track': 'DOM extraction allowed'}

    summaries['public_table_extract'] = client.measure('public_table_extract', lambda i: load('/tables'), table, runs, 0)

    def session_prepare(index):
        login(login_prepare(index), index)
        client.js("window.__benchmarkBeforeReload=true;'marked'")

    def reload(_, index):
        client.browser('reload')
        assert client.browser('wait', '--js', "typeof window.__benchmarkBeforeReload==='undefined' && document.readyState==='complete'", '--timeout-ms', '20000')
        assert client.browser('wait', '--condition', json.dumps({'UrlExact': base + '/secure'}))
        assert client.browser('wait', '--text', 'Welcome to the Secure Area')
        return {'authenticated_after_reload': True}

    summaries['public_session_reload'] = client.measure('public_session_reload', session_prepare, reload, runs, 0)
    return summaries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=Path('target/release/flowmux'))
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--samples', type=int, default=100)
    parser.add_argument('--warmup', type=int, default=5)
    parser.add_argument('--public', action='store_true')
    parser.add_argument('--self-test', action='store_true')
    args = parser.parse_args()
    assert percentile(list(range(1, 101)), .95) == 95
    assert summarize([{'phase': 'measurement', 'success': False}]) == {'attempted': 1, 'passed': 0, 'failed': 1, 'warmup_failed': 0}
    check = [{'phase': 'measurement', 'success': True, 'elapsed_ms': value, 'calls': 1, 'response_bytes': 10} for value in range(1, 101)]
    assert summarize(check)['p50_ms'] == 50.5 and summarize(check)['p95_ms'] == 95
    assert 'p95_ms' not in summarize(check[:99])
    if args.self_test:
        with tempfile.TemporaryDirectory(prefix='fm-bench-check-') as directory:
            client = Browser(None, None, {}, Path(directory))
            result = client.measure('stage_reset', lambda _: None, lambda _, i: None, 1, 0)
            assert result['passed'] == 1 and client.stage == 'setup'
        print('BENCHMARK_SELF_TEST_OK')
        return
    assert args.samples > 0 and args.warmup >= 0
    args.output.mkdir(parents=True, exist_ok=False)
    binary = args.binary.resolve()
    summary = {'started_at': datetime.now(timezone.utc).isoformat(),
               'commit': subprocess.check_output(['git', 'rev-parse', 'HEAD'], text=True).strip(),
               'binary_sha256': hashlib.file_digest(binary.open('rb'), 'sha256').hexdigest(),
               'samples_per_case': args.samples, 'warmup_per_case': 0 if args.public else args.warmup,
               'timer': 'perf_counter_ns, CLI spawn through final-state verification; setup excluded',
               'p95': 'nearest rank, only for at least 100 successful samples', 'cases': {}}
    try:
        with isolated_browser(binary, args.output) as (client, url, root, terminal):
            summary['cases'] = (public_cases(client, args.samples) if args.public else
                                local_cases(client, url, root, terminal, args.samples, args.warmup))
    except Exception as error:
        summary['harness_error'] = f'{type(error).__name__}: {error}'
        raise
    finally:
        summary['finished_at'] = datetime.now(timezone.utc).isoformat()
        (args.output / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    if any(case['failed'] or case['warmup_failed'] for case in summary['cases'].values()):
        raise SystemExit(1)


if __name__ == '__main__':
    main()
