#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise resized VT caches and overlapping large frames in an isolated Host/GUI."""

from agent_host_protocol import SOCKET_NAME

import argparse
import importlib.util
from pathlib import Path
import subprocess
import sys
import time
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location("host_fixture", ROOT / "scripts/test-agent-host-gui.py")
host_fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(host_fixture)
fixture = host_fixture.fixture

PROVIDER = r'''
import os, sys, time, tty
from pathlib import Path
root = Path(sys.argv[1])
tty.setraw(0)
(root / 'starts').write_text(str(os.getpid()))
def wait(name):
    while not (root / name).exists(): time.sleep(0.02)
def ready(name):
    print('\x1b[6n', end='', flush=True)
    reply = b''
    while not reply.endswith(b'R'): reply += os.read(0, 256)
    (root / name).touch()
print('x\r\n' * 1500, end='')
ready('warm')
wait('fill')
for index in range(4096):
    print('\x1b]2;' + f'{index:04}' + 't' * 4092 + '\x07\x1b[22;0t', end='')
marks = '\U0001d185' * 32
line = ''.join('\x1b[0;1;2;3;4:3;7;8;9;38;2;' + str(column % 255)
               + ';1;2;48;2;3;4;5;58;2;6;7;8mX' + marks for column in range(500)) + '\r\n'
for _ in range(1500): print(line, end='')
for column in range(500):
    print('\x1b[1;' + str(column + 1) + 'H\x1b]8;;https://example.com/'
          + f'{column:03}/' + 'l' * 4000 + '\x1b\\X' + marks, end='')
print('\x1b]8;;\x1b\\\x1b[?1049h', end='')
for _ in range(1500): print(line, end='')
print('\x1b[?1049l', end='')
ready('filled')
for index, text in [(1, 'A'), (2, 'B')]:
    wait('advance' + str(index))
    print('\x1b[1;1H' + text, end='')
    ready('advanced' + str(index))
pending = b''
while True:
    pending += os.read(0, 4096)
    if b'\r' not in pending and b'\n' not in pending: continue
    text = pending.decode().strip()
    pending = b''
    print('\x1b[0m\r\nACK:<' + text + '>\r\n', end='', flush=True)
    if text == 'quit': break
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
    clients = []
    host = None
    agent = None
    host_socket = h.root / "run" / SOCKET_NAME
    try:
        h.start_display()
        app, gui_socket = h.window('terminal-budget')
        with (h.root / 'host.log').open('w') as log:
            host = h.spawn([args.host, '--agent-host', '--socket', str(host_socket)],
                           stdout=log, stderr=log, cwd=h.root)
        fixture.wait_for(host_socket.exists, 'private Host socket')

        def client():
            connection = host_fixture.HostClient(host_socket, 'relay')
            clients.append(connection)
            return connection

        active = client()
        active.stream.settimeout(60)
        provider = h.root / 'budget-provider.py'
        provider.write_text(PROVIDER)
        agent = active.call('launch', launch={
            'request_id': host_fixture.launch_request_id(), 'provider': 'fixture',
            'label': 'terminal budget', 'argv': ['/usr/bin/python3', '-u', str(provider), str(h.root)],
            'cwd': str(h.root), 'env': list(h.env.items()), 'size': {'rows': 79, 'cols': 499},
        })['agent']
        hook = host_fixture.HostClient(host_socket, 'hook')
        clients.append(hook)
        for seq in range(1, 301):
            receipt = hook.call('lifecycle', instance=agent['instance'], epoch=agent['epoch'],
                                pid=agent['pid'], process_start=agent['process_start'],
                                session_id='budget-session', seq=seq,
                                event={'event': 'wait_resolved', 'item_id': f'{seq}:' + 'x' * 60_000})
            if not receipt['applied']:
                break
        else:
            raise AssertionError('Host retained unbounded lifecycle IDs')
        presence = next(a for a in active.call('list')['agents']
                        if a['instance'] == agent['instance'])['presence']
        assert presence['status'] == 'unknown', presence
        print(f'LIFECYCLE_BUDGET_OVERFLOW_SEQUENCE={seq}', flush=True)
        fixture.wait_for((h.root / 'warm').exists, 'initial history')
        prepared = active.call('prepare_attach', instance=agent['instance'], epoch=agent['epoch'],
                               request_id=str(uuid.uuid4()), view=None, require_gui=False, replace=None)
        key = prepared['key']
        attached = active.call('commit_attach', key=key, size={'rows': 79, 'cols': 499})
        active.call('view_ready', key=key, sequence=attached['frame']['sequence'])
        active.call('resize', key=key, size={'rows': 79, 'cols': 500})
        active.call('resize', key=key, size={'rows': 80, 'cols': 500})
        (h.root / 'fill').touch()
        print('FILL_STARTED', flush=True)
        heartbeat = time.monotonic()
        heartbeat_delays = []

        def filled():
            nonlocal heartbeat
            if (h.root / 'filled').exists():
                return True
            if time.monotonic() - heartbeat >= 2:
                try:
                    started = time.monotonic()
                    active.call('keep_alive')
                    heartbeat_delays.append(time.monotonic() - started)
                except OSError as error:
                    raise AssertionError('Host heartbeat failed during bulk output') from error
                heartbeat = time.monotonic()
            return False

        fixture.wait_for(filled, 'both grids and resized caches filled', timeout=180)
        print('FILL_COMPLETE', flush=True)
        assert heartbeat_delays, 'bulk output completed without a control request'
        longest = max(heartbeat_delays)
        print(f'HOST_HEARTBEAT_MAX_SECONDS={longest:.6f}', flush=True)
        assert longest < 1, f'bulk output delayed control beyond the GUI deadline: {longest}'
        first = active.call('frame', key=key, after=0, offset=0)['frame']
        active.call('resize', key=key, size={'rows': 79, 'cols': 500})
        (h.root / 'advance1').touch()
        fixture.wait_for((h.root / 'advanced1').exists, 'first screen update')
        pending = client()
        replacement = pending.call('prepare_attach', instance=agent['instance'], epoch=agent['epoch'],
                                   request_id=str(uuid.uuid4()), view=None, require_gui=False, replace=key)
        assert replacement['frame']['sequence'] > first['sequence']
        active.call('resize', key=key, size={'rows': 80, 'cols': 500})
        (h.root / 'advance2').touch()
        fixture.wait_for((h.root / 'advanced2').exists, 'second screen update')
        current = active.call('frame', key=key, after=0, offset=0)['frame']
        assert current['sequence'] > replacement['frame']['sequence']
        assert current['next_offset'] is not None
        peak = int(next(line.split()[1] for line in Path(f'/proc/{host.pid}/status').read_text().splitlines()
                        if line.startswith('VmHWM:')))
        print(f'TERMINAL_BUDGET_PEAK_KIB={peak}', flush=True)
        assert peak < 384 * 1024, peak
        h.pass_check('same Host retains bounded lifecycle IDs and large active/pending frames within one execution reservation')
        pending.call('abort_attach', key=replacement['key'])
        pending.close()
        active.close()
        observer = client()
        fixture.wait_for(lambda: next(a for a in observer.call('list')['agents']
                                     if a['instance'] == agent['instance'])['attachment'] is None,
                         'relay attachment released')
        h.rpc(gui_socket, 'workspace_create', root=str(h.root), name='terminal budget')
        pane = h.tree(gui_socket)[0]['panes'][0]['id']
        h.rpc(gui_socket, 'agent_view_open', pane=pane, instance=agent['instance'])
        token = '한글 입력 ' + uuid.uuid4().hex[:6]
        h.send(gui_socket, pane, token)
        fixture.wait_for(lambda: 'ACK:<' + token + '>' in h.screen(gui_socket, pane),
                         'fresh Hangul input through the live VTE', timeout=30)
        current_agent = next(a for a in observer.call('list')['agents'] if a['instance'] == agent['instance'])
        assert (current_agent['pid'], current_agent['process_start']) == (agent['pid'], agent['process_start'])
        assert (h.root / 'starts').read_text() == str(agent['pid'])
        h.pass_check('original provider reconnects in the real GUI and acknowledges fresh Hangul input with one spawn')
        h.send(gui_socket, pane, 'quit')
        host.wait(timeout=15)
        assert host.returncode == 0 and not host_socket.exists()
        h.pass_check('natural exit reaps the provider and empty Host')
        h.assert_protected()
        print('AGENT_TERMINAL_BUDGET_OK', flush=True)
    finally:
        if agent is not None and host is not None and host.poll() is None:
            try:
                connection = client()
                connection.call('terminate', instance=agent['instance'], epoch=agent['epoch'], mode='terminate')
                host.wait(timeout=15)
            except (OSError, AssertionError, TimeoutError, subprocess.TimeoutExpired):
                pass
        for connection in clients:
            connection.close()
        h.cleanup()


if __name__ == '__main__':
    main()
