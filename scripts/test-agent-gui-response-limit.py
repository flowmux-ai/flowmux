#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check bounded GUI forwarding, no effect replay, and same-provider survival."""

import argparse
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location('host_fixture', ROOT / 'scripts/test-agent-host-gui.py')
host_fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host_fixture)
fixture = host_fixture.fixture

PROVIDER = r'''
import json, os, socket, subprocess, sys, time
from pathlib import Path
root = Path(sys.argv[1])
while not (root / 'browser-pane').exists(): time.sleep(.05)
pane = (root / 'browser-pane').read_text()
for name, source in [
    ('small', "'한글'.repeat(1000)"),
    ('large', "globalThis.fmOnce=(globalThis.fmOnce||0)+1; 'x'.repeat(5*1024*1024)"),
    ('count', 'globalThis.fmOnce'),
]:
    result = subprocess.run([sys.argv[2], '--json', 'browser', 'eval', 'pane:' + pane, source],
                            capture_output=True, text=True, timeout=30)
    data = {'code': result.returncode, 'stdout_len': len(result.stdout), 'stderr': result.stderr[:1000]}
    if len(result.stdout) < 1024: data['stdout'] = result.stdout
    (root / (name + '.json')).write_text(json.dumps(data))
streams = []
admitted = rejected = 0
for index in range(56):
    stream = socket.socket(socket.AF_UNIX)
    stream.settimeout(20)
    stream.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
    stream.connect(os.environ['FLOWMUX_SOCKET_PATH'])
    def call(identifier, request):
        stream.sendall((json.dumps({'id': identifier, 'kind': 'request', 'verb': 'agent_host',
                                   'request': request}) + '\n').encode())
        data = b''
        while not data.endswith(b'\n'): data += stream.recv(4096)
        return json.loads(data)['agent_host']['response']
    call(1, {'op': 'hello', 'version': 18, 'role': 'relay'})
    context = call(2, {'op': 'resolve_context', 'instance': os.environ['FLOWMUX_AGENT_INSTANCE_ID'],
                      'epoch': os.environ['FLOWMUX_AGENT_HOST_EPOCH'], 'session_id': None})
    request = {'op': 'dispatch_context', 'context': {'key': context['key'], 'location': context['location']},
               'session_id': None, 'request': {'verb': 'browser_eval', 'pane': pane,
               'source': "globalThis.fmPressure=(globalThis.fmPressure||0)+1; 'x'.repeat(4*1024*1024-1024)"}}
    stream.sendall((json.dumps({'id': 3, 'kind': 'request', 'verb': 'agent_host', 'request': request}) + '\n').encode())
    prefix = stream.recv(1024)
    if b'"error"' in prefix:
        while not prefix.endswith(b'\n'): prefix += stream.recv(4096)
        assert 'request not started' in json.loads(prefix)['error']['message'], prefix
        rejected += 1
        stream.close()
    else:
        assert b'"agent_host"' in prefix, prefix
        admitted += 1
        streams.append(stream)
(root / 'pressure.json').write_text(json.dumps({'admitted': admitted, 'rejected': rejected}))
while not (root / 'release-pressure').exists(): time.sleep(.02)
for stream in streams: stream.close()
deadline = time.monotonic() + 20
while True:
    result = subprocess.run([sys.argv[2], '--json', 'browser', 'eval', 'pane:' + pane, 'globalThis.fmPressure'],
                            capture_output=True, text=True, timeout=30)
    if result.returncode == 0: break
    assert 'request not started' in result.stderr, result.stderr
    assert time.monotonic() < deadline, 'forwarding capacity was not released'
    time.sleep(.02)
(root / 'recovery.json').write_text(result.stdout)
print('FORWARD_DONE', flush=True)
for line in sys.stdin:
    print('ACK:' + line.strip(), flush=True)
    if line.strip() == 'quit': break
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default=str(ROOT / 'target/debug/flowmux'))
    parser.add_argument('--cli', default=str(ROOT / 'target/debug/flowmuxctl'))
    parser.add_argument('--host', default=str(ROOT / 'target/debug/flowmux-daemon'))
    parser.add_argument('--protected-pid', type=int, action='append', default=[])
    args = parser.parse_args()
    for name in ('gui', 'cli', 'host'):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = fixture.Harness(args)
    host = None
    observer = None
    agent = None
    try:
        h.start_display()
        _, gui_socket = h.window('forward-response')
        host_socket = h.root / 'run/flowmux-agent-host.sock'
        with (h.root / 'host.log').open('w') as log:
            host = h.spawn([args.host, '--agent-host', '--socket', str(host_socket)],
                           stdout=log, stderr=log, cwd=h.root)
        fixture.wait_for(host_socket.exists, 'private Host socket')
        observer = host_fixture.HostClient(host_socket, 'relay')
        provider = h.root / 'provider.py'
        provider.write_text(PROVIDER)
        agent = observer.call('launch', launch={
            'request_id': host_fixture.launch_request_id(), 'provider': 'fixture',
            'label': 'response bound', 'argv': ['/usr/bin/python3', '-u', str(provider), str(h.root), args.cli],
            'cwd': str(h.root), 'env': list(h.env.items()), 'size': {'rows': 24, 'cols': 80},
        })['agent']
        h.rpc(gui_socket, 'workspace_create', root=str(h.root), name='response bound')
        pane = h.tree(gui_socket)[0]['panes'][0]['id']
        h.rpc(gui_socket, 'agent_view_open', pane=pane, instance=agent['instance'])
        browser = h.rpc(gui_socket, 'browser_open', url='about:blank', target_pane=pane,
                        direction='vertical')['browser_pane_opened']['pane']
        (h.root / 'browser-pane').write_text(browser)
        fixture.wait_for(lambda: (h.root / 'count.json').exists(), 'forwarded evaluations', timeout=70)
        results = {name: json.loads((h.root / (name + '.json')).read_text())
                   for name in ('small', 'large', 'count')}
        print(json.dumps(results, ensure_ascii=False), flush=True)
        assert results['small']['code'] == 0 and results['small']['stdout_len'] > 2000, results
        large = results['large']
        assert large['code'] != 0 and 'exceeded 4194304 bytes' in large['stderr'], results
        assert 'outcome unknown' in large['stderr'] and 'not retried' in large['stderr'], results
        assert large['stdout_len'] < 1024 and 'error' in json.loads(large['stdout']), results
        count = results['count']
        assert count['code'] == 0 and json.loads(count['stdout'])['browser_result']['value'] == '1', results
        h.pass_check('small reply succeeds; oversized reply fails explicitly; GUI effect occurs once')
        fixture.wait_for(lambda: (h.root / 'pressure.json').exists(), 'slow response clients', timeout=70)
        pressure = json.loads((h.root / 'pressure.json').read_text())
        peak = int(next(line.split()[1] for line in Path(f'/proc/{host.pid}/status').read_text().splitlines()
                        if line.startswith('VmHWM:')))
        print(json.dumps({'slow_responses': pressure, 'host_peak_kib': peak}), flush=True)
        assert peak < 384 * 1024, peak
        started = time.monotonic()
        observer.call('list')
        assert time.monotonic() - started < 1, 'slow replies blocked Host control'
        effects = h.rpc(gui_socket, 'browser_eval', pane=browser, source='globalThis.fmPressure')['browser_result']['value']
        assert int(effects) == pressure['admitted'], (effects, pressure)
        assert pressure == {'admitted': 7, 'rejected': 49}, pressure
        h.pass_check('slow clients retain at most seven forwarded replies; rejection precedes GUI effects and control stays responsive')
        (h.root / 'release-pressure').touch()
        fixture.wait_for(lambda: (h.root / 'recovery.json').exists(), 'forwarding capacity reclaimed', timeout=35)
        assert int(json.loads((h.root / 'recovery.json').read_text())['browser_result']['value']) == 7
        h.pass_check('closing slow clients releases forwarding capacity without replaying rejected effects')
        current = next(a for a in observer.call('list')['agents'] if a['instance'] == agent['instance'])
        assert (current['pid'], current['process_start']) == (agent['pid'], agent['process_start'])
        h.send(gui_socket, pane, '한글 입력')
        fixture.wait_for(lambda: 'ACK:한글 입력' in h.screen(gui_socket, pane), 'same provider fresh input')
        h.pass_check('original provider survives reply rejection and accepts fresh Hangul input')
        h.send(gui_socket, pane, 'quit')
        host.wait(timeout=15)
        assert host.returncode == 0 and not host_socket.exists()
        h.assert_protected()
        h.pass_check('natural exit reaps the provider and empty Host')
        print('AGENT_GUI_RESPONSE_LIMIT_OK', flush=True)
    except Exception:
        if 'gui_socket' in locals() and 'pane' in locals():
            try:
                (h.root / 'failure-screen.txt').write_text(h.screen(gui_socket, pane))
            except (OSError, RuntimeError):
                pass
        raise
    finally:
        (h.root / 'release-pressure').touch()
        if observer:
            if agent is not None and host is not None and host.poll() is None:
                try:
                    observer.call('terminate', instance=agent['instance'], epoch=agent['epoch'], mode='terminate')
                    host.wait(timeout=15)
                except (OSError, AssertionError, subprocess.TimeoutExpired):
                    pass
            observer.close()
        h.cleanup()


if __name__ == '__main__':
    main()
