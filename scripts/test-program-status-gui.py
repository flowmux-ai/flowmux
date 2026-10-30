#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise program-status delivery through a disposable GUI's real PTY wrapper.

Requires an explicit isolated socket. These are protocol fixtures, not real
agent evidence. Leaves the exact snapshots and child output under /tmp.
"""

import argparse
import json
import os
from pathlib import Path
import shlex
import signal
import socket
import tempfile
import time


CHILD = r'''
import json, os, select, sys, termios, time, tty
from pathlib import Path
root = Path(sys.argv[1])
(root / 'child.json').write_text(json.dumps({'pid': os.getpid(), 'generation_exposed': 'FLOWMUX_PROGRAM_STATUS_GENERATION' in os.environ}))
def emit(payload):
    os.write(1, b'\x1b]' + payload.encode() + b'\x1b\\')
emit('7501;state=working:app=fixture')
emit('7501;state=blocked:id=child:kind=question')
deadline = time.monotonic() + 90
while time.monotonic() < deadline:
    if not select.select([sys.stdin], [], [], .2)[0]: continue
    line = sys.stdin.readline().strip()
    if line == 'clear-child': emit('7501;state=clear:id=child')
    elif line == 'invalid':
        emit('7501;state=clear:msg=AA==')
        emit('7501;state=done:id=bad/name/')
    elif line == 'prompt':
        emit('7501;state=done:id=result')
        emit('7501;state=error:id=error')
        emit('7501;state=blocked:id=child')
        emit('133;A')
    elif line == 'screens': os.write(1, b'\x1b[?1049h\x1b[!p\x1b[?1049l')
    elif line == 'reset': os.write(1, b'\x1bc')
    elif line == 'crash-ready': emit('7501;state=done:id=result')
    elif line == 'flood':
        for i in range(1000): emit(f'7501;state=working:id=job{i}:progress={i % 101}')
        emit('7501;state=clear')
        emit('7501;state=blocked:kind=permission:app=fixture')
    elif line == 'probe':
        saved = termios.tcgetattr(0); data = b''
        try:
            tty.setraw(0)
            os.write(1, b'\x1b]7501;?\x1b\\\x1b[c')
            until = time.monotonic() + 1
            while time.monotonic() < until:
                if select.select([0], [], [], max(0, until-time.monotonic()))[0]: data += os.read(0, 4096)
        finally: termios.tcsetattr(0, termios.TCSANOW, saved)
        (root / 'probe.json').write_text(json.dumps({'reply': data.hex(), 'status_reply': b']7501;' in data, 'sentinel': b'\x1b[?' in data and b'c' in data}))
    elif line == 'finish':
        emit('7501;state=done:app=fixture')
        emit('7501;state=working:id=unfinished')
        break
    (root / 'last-command').write_text(line)
print('PROGRAM_STATUS_CHILD_EXIT', flush=True)
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--socket', required=True)
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix='fm-program-', dir='/tmp'))
    print(f'PROGRAM STATUS ARTIFACTS: {root}', flush=True)
    script = root / 'child.py'
    script.write_text(CHILD)

    def rpc(verb, allow_error=False, **fields):
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(15)
            stream.connect(args.socket)
            stream.sendall((json.dumps(dict(id=1, kind='request', verb=verb, **fields))+'\n').encode())
            result = json.loads(stream.makefile().readline())
        if not allow_error: assert 'error' not in result, result
        return result

    original_workspace = rpc('workspace_current')['workspace_current']['id']
    if original_workspace is None:
        original_workspace = rpc('workspace_create', name='Protocol control', root=str(root))['workspace_created']['id']
    workspace = rpc('workspace_create', name='Program status transport', root=str(root))['workspace_created']['id']
    tree = rpc('workspace_tree')['tree']['workspaces']
    ws = next(w for w in tree if w['id'] == workspace)
    pane = ws['panes'][0]['id']; surface = ws['panes'][0]['tabs'][0]['id']
    if original_workspace is not None:
        rpc('workspace_focus', workspace=original_workspace)
    before_focus = rpc('workspace_current')['workspace_current']['id']

    def current():
        return rpc('terminal_program_status_query', surface=surface)['terminal_program_status']['snapshot']

    def display():
        entries = rpc('agent_explain', surface=surface)['agent_explanations']['agents']
        assert len(entries) == 1, entries
        return entries[0]

    def wait(predicate, label):
        end = time.monotonic()+15
        last = None
        while time.monotonic() < end:
            last = current()
            if predicate(last):
                (root / (label+'.json')).write_text(json.dumps(last, indent=2))
                return last
            time.sleep(.05)
        (root / (label+'-failure.json')).write_text(json.dumps({
            'snapshot': last,
            'tree': rpc('workspace_tree'),
            'screen': rpc('pane_read_screen', pane=pane),
        }, indent=2))
        raise AssertionError((label, last))

    def send(command):
        rpc('pane_send_keys', pane=pane, keys=command+'\n')
        end = time.monotonic()+15
        while not (root/'last-command').exists() or (root/'last-command').read_text() != command:
            assert time.monotonic() < end, command
            time.sleep(.05)

    rpc('pane_send_keys', pane=pane, keys='exec python3 '+shlex.quote(str(script))+' '+shlex.quote(str(root))+'\n')
    first = wait(lambda s: s and len(s['records']) == 2, 'initial')
    assert not json.loads((root/'child.json').read_text())['generation_exposed']
    assert any('kind=question' in record for record in first['records'])
    shown = display()
    assert shown['status'] == 'blocked' and shown['attention'] == 'input_required', shown
    assert shown['terminal_report_primary'] and shown['status_evidence']['source'] == 'program', shown
    assert shown['pending_waits'][0]['reason'] == 'question', shown
    assert shown['terminal_report']['records'][1]['app'] == 'fixture', shown
    (root/'initial-display.json').write_text(json.dumps(shown, indent=2))
    assert display() == shown, 'details read changed the projected state'
    assert rpc('workspace_current')['workspace_current']['id'] == before_focus
    for _ in range(10): assert current() == first, 'query changed state'

    # Exact replay is idempotent; malformed and foreign owners cannot mutate it.
    rpc('terminal_program_status', surface=surface, snapshot=first)
    for changed in [dict(owner_pid=first['owner_pid']+1), dict(generation='00000000-0000-0000-0000-000000000001'), dict(records=['7501;state=clear'])]:
        bad = dict(first, revision=first['revision']+1, **changed)
        response = rpc('terminal_program_status', allow_error=True, surface=surface, snapshot=bad)
        assert 'error' in response, response
        assert current() == first

    send('invalid'); time.sleep(.2)
    assert current() == first, 'invalid report partially changed records'
    send('clear-child')
    cleared = wait(lambda s: s and len(s['records']) == 1, 'child-cleared')
    rpc('terminal_program_status', surface=surface, snapshot=first)
    assert current() == cleared, 'delayed snapshot revived the child'
    send('prompt')
    results = wait(lambda s: s and s['records'] == ['7501;state=done:id=result', '7501;state=error:id=error'], 'prompt-boundary')
    assert display()['attention'] == 'failure_review', display()
    send('screens'); time.sleep(.2)
    assert current() == results, 'screen switch or soft reset erased records'
    send('reset'); wait(lambda s: s and not s['records'], 'full-reset')
    send('flood')
    flooded = wait(lambda s: s and s['records'] == ['7501;state=blocked:app=fixture:kind=permission'], 'coalesced-flood')
    assert display()['attention'] == 'input_required' and display()['pending_waits'][0]['reason'] == 'approval', display()
    send('probe')
    probe = json.loads((root/'probe.json').read_text())
    assert probe['sentinel'] and not probe['status_reply'], probe
    assert current() == flooded, 'probe changed records'
    rpc('pane_send_keys', pane=pane, keys='finish\n')
    final = wait(lambda s: s and s['records'] == ['7501;state=done:app=fixture'], 'process-exit')
    end = time.monotonic()+10
    while time.monotonic() < end:
        response = rpc('terminal_program_status', allow_error=True, surface=surface, snapshot=dict(final, revision=final['revision']+1))
        if 'error' in response: break
        time.sleep(.1)
    else: raise AssertionError('ended PTY still accepted reports')
    assert current()['records'] == final['records']
    assert display()['turn_outcome'] == 'unknown' and not display()['pending_waits'], display()
    assert rpc('workspace_current')['workspace_current']['id'] == before_focus

    # Kill only the wrapper just created for this fixture. The GUI's own child
    # exit observation must clear live records even without a final IPC snapshot.
    original_surface = surface
    extra = rpc('surface_create', workspace=workspace, cwd=str(root))['surface_created']
    pane, surface = extra['pane'], extra['id']
    crash_root = root/'crash'; crash_root.mkdir()
    rpc('pane_send_keys', pane=pane, keys='exec python3 '+shlex.quote(str(script))+' '+shlex.quote(str(crash_root))+'\n')
    crash = wait(lambda s: s and len(s['records']) == 2, 'crash-initial')
    rpc('pane_send_keys', pane=pane, keys='crash-ready\n')
    wait(lambda s: s and len(s['records']) == 3, 'crash-ready')
    child_pid = json.loads((crash_root/'child.json').read_text())['pid']
    assert crash['owner_pid'] != child_pid
    os.kill(crash['owner_pid'], signal.SIGKILL)
    wait(lambda s: s and s['records'] == ['7501;state=done:id=result'], 'wrapper-crash')
    # The other tab keeps this pane alive, so closing the ended fixture cannot
    # close the last pane or any original workspace.
    rpc('surface_close', pane=pane, surface=surface)
    assert current() is None, 'closed surface retained program records'
    assert rpc('terminal_program_status_query', surface=original_surface)['terminal_program_status']['snapshot']['records'] == final['records']
    if original_workspace is not None: rpc('workspace_focus', workspace=original_workspace)
    print('PROGRAM_STATUS_TRANSPORT_OK', flush=True)


if __name__ == '__main__':
    main()
