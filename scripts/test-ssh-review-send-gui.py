#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify SSH review Send through real PTYs in an isolated GUI.

Uses ssh-workspace-fixture.py --keep; no Codex credentials or model calls.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import shlex
import sys
import time
import uuid

sys.dont_write_bytecode = True
repo = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('gui', repo / 'scripts/test-ssh-workspace-gui.py')
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--gui', default='target/debug/flowmux')
parser.add_argument('--cli', default='target/debug/flowmuxctl')
parser.add_argument('--ssh-config', required=True)
parser.add_argument('--remote-cwd', required=True)
parser.add_argument('--host', default='flowmux-fixture')
parser.add_argument('--protected-pid', type=int, action='append', default=[])
args = parser.parse_args()
for key in ('gui', 'cli', 'ssh_config'):
    setattr(args, key, str(Path(getattr(args, key)).resolve(strict=True)))
args.remote_cwd += '/review-send-' + uuid.uuid4().hex
h = gui.Harness(args)
h.env.update(GTK_A11Y='atspi', GTK_USE_PORTAL='0')
h.env.pop('AT_SPI_BUS_ADDRESS', None)
d = None
receiver = r'''import json, os, pathlib, select, sys, termios, time, tty
control, receipt = map(pathlib.Path, sys.argv[1:3])
previous = termios.tcgetattr(0)
tty.setraw(0)
last = None
data = b''
deadline = time.monotonic() + 300
try:
    os.write(1, b'\x1b[?1049h\x1b[?2004h')
    while time.monotonic() < deadline:
        value = json.loads(control.read_text())
        if value.get('exit'): break
        if value != last:
            mode = value['mode']
            title = value['title']
            body = 'OpenAI Codex\r\n'
            if mode == 'working': body += 'Working (1s • esc to interrupt)\r\n'
            if mode == 'blocked':
                title = 'Codex Action Required'
                body += 'Do you want to approve this command?\r\n'
            # A painted placeholder wraps in the narrow review split and puts
            # this fixture's cursor on its continuation row. Draw an empty
            # composer so the Send test exercises the empty-input contract.
            body += '› existing user draft' if mode == 'draft' else '› '
            cursor = '\r\x1b[21C' if mode == 'draft' else '\r\x1b[2C'
            if title == 'Codex source':
                title, body, cursor = 'Source receiver', 'Source receiver\r\n> ', ''
            os.write(1, ('\x1b[2J\x1b[3J\x1b[H\x1b]2;' + title + '\x07' + body + cursor).encode())
            last = value
        if select.select([0], [], [], .05)[0]:
            chunk = os.read(0, 65536)
            if not chunk: break
            data += chunk
            temporary = receipt.with_suffix('.tmp')
            temporary.write_bytes(data)
            temporary.replace(receipt)
finally:
    os.write(1, b'\x1b[?2004l\x1b[?1049l')
    termios.tcsetattr(0, termios.TCSANOW, previous)
'''

def remote_python(program):
    return h.remote('python3 -c ' + shlex.quote(program)).stdout

def frame(label, mode='idle'):
    value = json.dumps({'title': 'Codex ' + label, 'mode': mode})
    remote_python('from pathlib import Path; path=Path(' + repr(args.remote_cwd + '/' + label + '.json') + '); temporary=path.with_name(path.name + ".tmp"); temporary.write_text(' + repr(value) + '); temporary.replace(path)')

def receipt(label):
    return bytes.fromhex(remote_python('from pathlib import Path; p=Path(' + repr(args.remote_cwd + '/' + label + '.bin') + '); print(p.read_bytes().hex() if p.exists() else "")').strip())

try:
    setup = '''import os, pathlib, subprocess
root=pathlib.Path(%r); root.mkdir(); os.chdir(root)
env=dict(os.environ,GIT_CONFIG_NOSYSTEM='1',GIT_CONFIG_GLOBAL='/dev/null')
def git(*a): return subprocess.check_output(['git',*a],env=env,text=True).strip()
git('init','--initial-branch=main','--template=')
git('config','user.name','Send test'); git('config','user.email','test@localhost')
(root/'app.py').write_text('before\\n'); git('add','app.py'); git('commit','-m','baseline')
(root/'app.py').write_text('committed\\n'); git('add','app.py'); git('commit','-m','historical change')
(root/'app.py').write_text('working\\n')
(root/'.git/info').mkdir(exist_ok=True)
(root/'.git/info/exclude').write_text('receiver.py\\n*.json\\n*.bin\\n*.tmp\\n')
(root/'receiver.py').write_text(%r)
print(git('rev-parse','HEAD'))
''' % (args.remote_cwd, receiver)
    oid = remote_python(setup).strip().splitlines()[-1]
    clean = h.root / 'clean-shell'
    clean.write_text('#!/bin/sh\nexec /bin/bash --noprofile --norc\n'); clean.chmod(0o755)
    config = h.root / 'config/flowmux'; config.mkdir()
    (config / 'options.json').write_text(json.dumps({'default_shell': str(clean), 'terminal_minimap_enabled': False, 'system_notifications_enabled': False}))
    h.start_display()
    os.environ['DISPLAY'] = h.env['DISPLAY']
    os.environ['DBUS_SESSION_BUS_ADDRESS'] = h.env['DBUS_SESSION_BUS_ADDRESS']
    os.environ.pop('AT_SPI_BUS_ADDRESS', None)
    import gi
    gi.require_version('Atspi', '2.0')
    from gi.repository import Atspi
    from Xlib import X, display
    from Xlib.ext import xtest
    Atspi.set_timeout(2000, 2000)
    process, sock = h.window('send-review')
    d = display.Display(h.env['DISPLAY'])
    workspace = h.create(sock)
    source, source_tab = h.terminals(h.workspace(sock, workspace))[0]
    target = h.rpc(sock, 'pane_split', pane=source, direction='vertical')['pane_split_done']['new_pane']
    target_tab = next(t for p, t in h.terminals(h.workspace(sock, workspace)) if p == target)
    for label, pane, tab in [('source', source, source_tab), ('target', target, target_tab)]:
        frame(label)
        h.marker(sock, pane, tab['id'], args.remote_cwd)
        h.send(sock, pane, 'exec python3 -u ' + shlex.join([args.remote_cwd + '/receiver.py', args.remote_cwd + '/' + label + '.json', args.remote_cwd + '/' + label + '.bin']))

    def agent(tab):
        return next(t for _, t in h.terminals(h.workspace(sock, workspace)) if t['id'] == tab['id']).get('agent')

    def expect_status(status):
        statuses = ('idle', 'done') if status == 'idle' else (status,)
        value = gui.wait_for(lambda: (value if (value := agent(target_tab)) and value['status'] in statuses else None), 'target status ' + status)
        assert value['source'] == 'flowmux:screen' and value.get('pid') is None and value.get('session_id') is None, value

    expect_status('idle')
    assert agent(source_tab) is None
    h.pass_check('Remote Codex receiver is detected through SSH without injected hook metadata')

    def walk(node):
        node.clear_cache()
        yield node
        for i in range(node.get_child_count()):
            child = node.get_child_at_index(i)
            if child: yield from walk(child)

    def nodes():
        desktop = Atspi.get_desktop(0); desktop.clear_cache()
        app = next((desktop.get_child_at_index(i) for i in range(desktop.get_child_count()) if desktop.get_child_at_index(i).get_process_id() == process.pid), None)
        return [] if app is None else [n for n in walk(app) if n.get_state_set().contains(Atspi.StateType.SHOWING)]

    def find(name, role=None):
        return gui.wait_for(lambda: next((n for n in nodes() if n.get_name() == name and (role is None or n.get_role() == role)), None), name, timeout=10)

    def click(node):
        action = node.get_action_iface()
        if node.get_role() not in (Atspi.Role.LABEL, Atspi.Role.LIST_ITEM) and action and action.get_n_actions():
            assert action.do_action(0), node.get_name()
        else:
            b = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
            if b.x == b.y == 0: b = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
            xtest.fake_input(d, X.MotionNotify, x=b.x + b.width // 2, y=b.y + b.height // 2)
            xtest.fake_input(d, X.ButtonPress, 1); xtest.fake_input(d, X.ButtonRelease, 1); d.sync()
        time.sleep(.2)

    def open_review():
        h.rpc(sock, 'surface_focus', pane=source, surface=source_tab['id'])
        click(find('Code Review', Atspi.Role.PUSH_BUTTON))
        find('Diff with old and new line numbers')

    def add_comment(text):
        click(find('File comment', Atspi.Role.PUSH_BUTTON))
        assert Atspi.EditableText.set_text_contents(find('Review comment'), text)
        click(find('Add comment', Atspi.Role.PUSH_BUTTON))
        find('Comment saved')

    def choose_target():
        button = gui.wait_for(lambda: next((n for n in nodes() if n.get_role() == Atspi.Role.TOGGLE_BUTTON and n.get_name().startswith('Send ·')), None), 'Send button')
        click(button)
        button = gui.wait_for(lambda: next((n for n in nodes() if n.get_role() == Atspi.Role.PUSH_BUTTON and n.get_name().startswith('codex ·')), None), 'remote target menu')
        print('TARGET:', button.get_name(), flush=True)
        return button

    def send():
        click(choose_target())

    open_review()
    note = 'SSH_SEND_WORKTREE: 검토해주세요.\n두 번째 줄도 한 번에 전달됩니다.'
    add_comment(note)
    for mode, status, message in [('draft', 'idle', 'already has input'), ('working', 'working', 'busy or waiting'), ('blocked', 'blocked', 'busy or waiting')]:
        frame('target', mode); expect_status(status)
        send()
        gui.wait_for(lambda: any(message in n.get_name() for n in nodes()), 'safe rejection ' + mode)
        assert receipt('source') == receipt('target') == b''
        h.pass_check('SSH Send refuses ' + mode + ' without changing either remote input')

    frame('target'); expect_status('idle'); send()
    data = gui.wait_for(lambda: data if (data := receipt('target')).endswith(b'\x1b[201~\r') else None, 'remote bracketed paste and Enter')
    assert data.startswith(b'\x1b[200~Code review feedback'), data
    assert note.replace('\n', '\r').encode() in data and args.remote_cwd.encode() in data, data
    assert b'Review target: Unstaged + Staged' in data and data.count(b'\x1b[200~') == 1, data
    assert receipt('source') == b''
    (h.root / 'working-receipt.bin').write_bytes(data)
    h.pass_check('Actual Send delivers Korean multiline worktree feedback to only the selected SSH PTY as one bracketed paste plus Enter')

    open_review()
    click(find('Unstaged + Staged', Atspi.Role.TOGGLE_BUTTON))
    click(find(oid[:8] + '  historical change', Atspi.Role.LABEL))
    find('historical change', Atspi.Role.LABEL)
    add_comment('SSH_SEND_COMMIT: 선택한 커밋을 검토해주세요.')
    before = len(data)
    send()
    second = gui.wait_for(lambda: data[before:] if len(data := receipt('target')) > before and data.endswith(b'\x1b[201~\r') else None, 'historical feedback')
    assert ('Review target: commit ' + oid).encode() in second, second
    assert 'SSH_SEND_COMMIT: 선택한 커밋을 검토해주세요.'.encode() in second, second
    assert b'SSH_SEND_WORKTREE' not in second and receipt('source') == b'', second
    (h.root / 'commit-receipt.bin').write_bytes(second)
    h.pass_check('Selected commit Send carries the remote commit OID and only that commit review')

    h.ssh(sock, 'disconnect', workspace=workspace)
    gui.wait_for(lambda: agent(target_tab) is None, 'disconnection removes remote target')
    h.pass_check('Disconnect removes the remote Send target instead of retaining the stale Codex screen')
    h.close_window(process)
except BaseException:
    if d:
        from PIL import Image
        screen = d.screen().root; g = screen.get_geometry()
        raw = screen.get_image(0, 0, g.width, g.height, X.ZPixmap, 0xffffffff)
        Image.frombytes('RGB', (g.width, g.height), raw.data, 'raw', 'BGRX').save(h.root / 'failure.png')
        (h.root / 'accessibility.txt').write_text('\n'.join(n.get_role_name() + ': ' + n.get_name() for n in nodes() if n.get_name()))
    raise
finally:
    if d: d.close()
    try:
        remote_python('import shutil; shutil.rmtree(' + repr(args.remote_cwd) + ', ignore_errors=True)')
    finally:
        h.cleanup()
