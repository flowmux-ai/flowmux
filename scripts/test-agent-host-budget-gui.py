#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Fill all seven execution reservations and both IPC pools in a private Host."""

import argparse
import contextlib
import importlib.util
import json
from pathlib import Path
import socket
import sys
import time
import uuid

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
spec = importlib.util.spec_from_file_location(
    'terminal_budget', ROOT / 'scripts/test-agent-terminal-budget.py')
budget = importlib.util.module_from_spec(spec)
spec.loader.exec_module(budget)
fixture = budget.fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('gui', 'cli', 'host'):
        binary = 'flowmux-daemon' if name == 'host' else 'flowmuxctl' if name == 'cli' else 'flowmux'
        parser.add_argument('--' + name, default=str(ROOT / 'target/debug' / binary))
    parser.add_argument('--protected-pid', type=int, action='append', default=[])
    args = parser.parse_args()
    for name in ('gui', 'cli', 'host'):
        setattr(args, name, str(Path(getattr(args, name)).resolve(strict=True)))
    h = fixture.Harness(args)
    # A relay that fails leaves its reason, not a backtrace, on the screen the failure record keeps.
    h.env['RUST_BACKTRACE'] = '0'
    clients = []
    executions = []
    observer = None
    connections = []
    with contextlib.ExitStack() as peers:
        try:
            h.start_display()
            _, gui_socket = h.window('host-budget')
            host_socket = h.root / 'run/flowmux-agent-host.sock'
            with (h.root / 'host.log').open('w') as log:
                host = h.spawn([args.host, '--agent-host', '--socket', str(host_socket)],
                               stdout=log, stderr=log, cwd=h.root)
            fixture.wait_for(host_socket.exists, 'private Host socket')

            def client():
                connection = budget.host_fixture.HostClient(host_socket, 'relay')
                clients.append(connection)
                return connection

            provider = h.root / 'budget-provider.py'
            provider.write_text(budget.PROVIDER)
            for index in range(7):
                root = h.root / str(index)
                root.mkdir()
                active = client()
                agent = active.call('launch', launch={
                    'request_id': budget.host_fixture.launch_request_id(), 'provider': 'fixture',
                    'label': f'host budget {index}',
                    'argv': ['/usr/bin/python3', '-u', str(provider), str(root)],
                    'cwd': str(root), 'env': list(h.env.items()),
                    'size': {'rows': 79, 'cols': 499},
                })['agent']
                fixture.wait_for((root / 'warm').exists, 'initial history')
                key = active.call('prepare_attach', instance=agent['instance'], epoch=agent['epoch'],
                                  request_id=str(uuid.uuid4()), view=None,
                                  require_gui=False, replace=None)['key']
                attached = active.call('commit_attach', key=key, size={'rows': 79, 'cols': 499})
                active.call('view_ready', key=key, sequence=attached['frame']['sequence'])
                active.call('resize', key=key, size={'rows': 79, 'cols': 500})
                active.call('resize', key=key, size={'rows': 80, 'cols': 500})
                executions.append((root, active, agent, key))
            for root, *_ in executions:
                (root / 'fill').touch()
            heartbeat = time.monotonic()

            def filled():
                nonlocal heartbeat
                if time.monotonic() - heartbeat >= 2:
                    for _, active, _, _ in executions:
                        active.call('keep_alive')
                    heartbeat = time.monotonic()
                return all((root / 'filled').exists() for root, *_ in executions)

            fixture.wait_for(filled, 'all seven rich dual grids filled', timeout=240)
            pending_frames = []
            for root, active, agent, key in executions:
                first = active.call('frame', key=key, after=0, offset=0)['frame']
                active.call('resize', key=key, size={'rows': 79, 'cols': 500})
                (root / 'advance1').touch()
                fixture.wait_for((root / 'advanced1').exists, 'first updated screen')
                pending = client()
                replacement = pending.call('prepare_attach', instance=agent['instance'],
                    epoch=agent['epoch'], request_id=str(uuid.uuid4()), view=None,
                    require_gui=False, replace=key)
                assert replacement['frame']['sequence'] > first['sequence']
                active.call('resize', key=key, size={'rows': 80, 'cols': 500})
                (root / 'advance2').touch()
                fixture.wait_for((root / 'advanced2').exists, 'second updated screen')
                current = active.call('frame', key=key, after=0, offset=0)['frame']
                assert current['sequence'] > replacement['frame']['sequence']
                assert current['next_offset'] is not None
                pending_frames.append((pending, replacement))

            dense = {'id': 1, 'kind': 'request', 'verb': 'ping',
                     'unused': ['x' * 31] * 30_000, 'padding': ''}
            dense['padding'] = 'p' * (1024 * 1024 - 1 - len(json.dumps(dense, separators=(',', ':'))))
            wire = (json.dumps(dense, separators=(',', ':')) + '\n').encode()
            assert len(wire) == 1024 * 1024
            counts = []
            admitted = []
            for endpoint, limit in ((host_socket, 256), (Path(str(host_socket) + '.ctl'), 16)):
                count = 0
                for _ in range(limit + 1):
                    peer = peers.enter_context(socket.socket(socket.AF_UNIX))
                    peer.settimeout(5)
                    peer.connect(str(endpoint))
                    reader = peers.enter_context(peer.makefile('rb'))
                    try:
                        peer.sendall(wire)
                        response = reader.readline()
                    except (BrokenPipeError, ConnectionResetError):
                        break
                    if not response:
                        break
                    assert 'pong' in json.loads(response)
                    admitted.append((peer, reader))
                    count += 1
                else:
                    raise AssertionError('IPC connection limit was not enforced')
                assert count > 0
                counts.append(count)
            assert counts[1] == 16, counts
            for peer, _ in admitted:
                peer.sendall(wire[:-1])
            for pending, replacement in pending_frames:
                frame = replacement['frame']
                continued = pending.call('frame', key=replacement['key'], after=frame['sequence'],
                                         offset=frame['next_offset'])['frame']
                assert continued['sequence'] == frame['sequence']
            peak = int(next(line.split()[1] for line in Path(f'/proc/{host.pid}/status').read_text().splitlines()
                            if line.startswith('VmHWM:')))
            print(f'HOST_BUDGET_PEAK_KIB={peak} executions=7 pending_frames=7 pools={counts}', flush=True)
            assert peak < 3 * 1024 * 1024, peak
            h.pass_check('all seven maximum-width dual grids, overlapping frames and both IPC pools fit the Host budget')
            peers.close()
            for pending, replacement in pending_frames:
                pending.call('abort_attach', key=replacement['key'])
                pending.close()
            for _, active, _, _ in executions:
                active.close()
            observer = client()
            h.rpc(gui_socket, 'workspace_create', root=str(h.root), name='host budget')
            pane = h.tree(gui_socket)[0]['panes'][0]['id']

            for root, _, agent, _ in executions:
                started = time.monotonic()
                view = h.rpc(gui_socket, 'agent_view_open', pane=pane,
                             instance=agent['instance'])['surface_created']['id']
                connections.append({'instance': agent['instance'],
                                    'elapsed_seconds': time.monotonic() - started})
                print('HOST_BUDGET_RECONNECT=' + json.dumps(connections[-1]), flush=True)
                h.wait_active(gui_socket, pane, view)
                token = '한글 ' + uuid.uuid4().hex[:6]
                h.send(gui_socket, pane, token)
                fixture.wait_for(lambda: 'ACK:<' + token + '>' in h.screen(gui_socket, pane), 'fresh input')
                current = next(a for a in observer.call('list')['agents'] if a['instance'] == agent['instance'])
                assert (current['pid'], current['process_start']) == (agent['pid'], agent['process_start'])
                assert (root / 'starts').read_text() == str(agent['pid'])
                h.send(gui_socket, pane, 'quit')
                fixture.wait_for(lambda: not Path(f"/proc/{agent['pid']}").exists(), 'provider reaped')
                # The window rebuilds an ended view's tab as a shell; open the next view after
                # that. The last one retires the Host.
                if agent is not executions[-1][2]:
                    fixture.wait_for(lambda: h.screen(gui_socket, pane).rstrip().endswith('$'),
                                     'ended view continues as a shell')
            host.wait(timeout=15)
            assert host.returncode == 0 and not host_socket.exists()
            h.pass_check('all seven original processes accept fresh GUI input, exit naturally and retire the empty Host')
            (h.root / 'host-budget-connections.json').write_text(json.dumps(connections, indent=2))
            h.assert_protected()
        except Exception as error:
            diagnostic = {'error': str(error)}
            for delay in (() if observer is None else (0, 5)):
                time.sleep(delay)
                try:
                    diagnostic[f'agents_after_{delay}s'] = observer.call('list')
                    screen = h.screen(gui_socket, pane)
                    diagnostic[f'screen_after_{delay}s'] = screen[:2000] + screen[-2000:]
                except Exception as capture_error:
                    diagnostic[f'capture_error_after_{delay}s'] = str(capture_error)
            # The screen is too narrow for a relay's reason; its log has the whole line.
            diagnostic['relay_log'] = [line for log in h.root.glob('state/flowmux/logs/flowmuxctl.log*')
                                       for line in log.read_text(errors='replace').splitlines()
                                       if 'agent_relay' in line][-20:]
            (h.root / 'host-budget-failure.json').write_text(json.dumps(diagnostic, indent=2))
            raise
        finally:
            for connection in clients:
                connection.close()
            h.cleanup()


if __name__ == '__main__':
    main()
