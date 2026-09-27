#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Read-only Korean input audit using real IBus/XTest in an isolated GUI.

Requires Xvfb, ibus-hangul, python3-xlib and Pillow. No user display or IBus
session is used. --nav=off explicitly disables WSL's automatic workaround.
Byte assertions measure the raw child input, not shell/TUI editing semantics.
Screenshots require separate visual review. Artifacts survive cleanup.
"""

import argparse
import importlib.util
import json
import os
from pathlib import Path
import select
import shlex
import socket as unix_socket
import subprocess
import sys
import time
from types import SimpleNamespace

from PIL import Image
from Xlib import X, XK, display
from Xlib.ext import xtest

sys.dont_write_bytecode = True
REPO = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default=str(REPO / 'target/fast/flowmux'))
    parser.add_argument('--cli', default=str(REPO / 'target/fast/flowmuxctl'))
    parser.add_argument('--sync', choices=['0', '1'], required=True)
    parser.add_argument('--nav', choices=['on', 'off'], required=True)
    args = parser.parse_args()
    spec = importlib.util.spec_from_file_location('fixture', REPO / 'scripts/test-ssh-workspace-gui.py')
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    protected = []
    for entry in Path('/proc').iterdir():
        try:
            if entry.name.isdigit() and (entry / 'comm').read_text().strip() == 'flowmux':
                protected.append(int(entry.name))
        except OSError:
            pass
    class AuditHarness(fixture.Harness):
        def start_display(self):
            # WSLg owns /tmp/.X11-unix. Use a private abstract socket without
            # changing that directory or accidentally connecting to display :0.
            number = 100 + os.getpid() % 30000
            address = '\0/tmp/.X11-unix/X' + str(number)
            assert not Path('/tmp/.X11-unix/X' + str(number)).exists()
            with unix_socket.socket(unix_socket.AF_UNIX) as probe:
                assert probe.connect_ex(address) != 0, 'private display already in use'
            with (self.root / 'xvfb.log').open('w') as log:
                server = self.spawn(['Xvfb', ':' + str(number), '-screen', '0',
                                     '1600x1000x24', '-nolisten', 'tcp', '-nolisten', 'unix'],
                                    stdout=log, stderr=log)

            def ready():
                assert server.poll() is None, 'private Xvfb exited'
                with unix_socket.socket(unix_socket.AF_UNIX) as probe:
                    return probe.connect_ex(address) == 0

            fixture.wait_for(ready, 'private Xvfb abstract socket')
            self.env['DISPLAY'] = ':' + str(number)
            with (self.root / 'dbus.log').open('w') as log:
                bus = self.spawn(['dbus-daemon', '--session', '--nofork', '--print-address=1'],
                                 stdout=subprocess.PIPE, stderr=log)
            assert select.select([bus.stdout], [], [], 10)[0], 'private D-Bus startup'
            self.env['DBUS_SESSION_BUS_ADDRESS'] = bus.stdout.readline().decode().strip()

    h = AuditHarness(SimpleNamespace(gui=args.gui, cli=args.cli, protected_pid=protected))
    h.env.update(GTK_USE_PORTAL='0', GTK_IM_MODULE='ibus',
                 IBUS_ENABLE_SYNC_MODE=args.sync, GSETTINGS_BACKEND='memory',
                 IBUS_ADDRESS='unix:path=' + str(h.root / 'ibus.sock'))
    h.env['FLOWMUX_ENABLE_IBUS_NAV_WORKAROUND' if args.nav == 'on'
          else 'FLOWMUX_NO_IBUS_NAV_WORKAROUND'] = '1'
    shell = h.root / 'shell'
    shell.write_text('#!/bin/sh\nexec /bin/bash --noprofile --norc\n')
    shell.chmod(0o755)
    (h.root / 'config/flowmux').mkdir()
    (h.root / 'config/flowmux/options.json').write_text(json.dumps({
        'default_shell': str(shell), 'terminal_minimap_enabled': False,
        'cursor_blink': False, 'system_notifications_enabled': False,
    }))
    sink = h.root / 'input_sink.py'
    sink.write_text('''# SPDX-License-Identifier: GPL-3.0-or-later
import json,os,select,sys,termios,time,tty
old=termios.tcgetattr(0)
tty.setraw(0)
try:
    os.write(1,b'\\x1b[?1004h\\x1b[?25l\\r\\nINPUT READY: ')
    with open(sys.argv[1],'w',buffering=1) as out:
        end=time.monotonic()+180
        while time.monotonic()<end:
            if select.select([0],[],[],.1)[0]:
                data=os.read(0,4096)
                if not data: break
                out.write(json.dumps({'time':time.monotonic(),'hex':data.hex()})+'\\n')
finally:
    termios.tcsetattr(0,termios.TCSANOW,old)
''')
    cases = []
    connection = None
    try:
        h.start_display()
        with (h.root / 'ibus.log').open('w') as log:
            h.spawn(['ibus-daemon', '--panel=disable', '--emoji-extension=disable',
                     '--address=' + h.env['IBUS_ADDRESS'], '--cache=none'], stdout=log, stderr=log)
        fixture.wait_for(lambda: (h.root / 'ibus.sock').exists(), 'private IBus')
        app, socket = h.window('korean-audit')
        ws = h.rpc(socket, 'workspace_create', name='Korean input audit', root=str(h.root))['workspace_created']['id']
        pane = h.workspace(socket, ws)['panes'][0]['id']
        connection = display.Display(h.env['DISPLAY'])

        def main_window():
            for window in connection.screen().root.query_tree().children:
                pid = window.get_full_property(connection.intern_atom('_NET_WM_PID'), X.AnyPropertyType)
                if window.get_attributes().map_state == X.IsViewable and pid is not None and int(pid.value[0]) == app.pid:
                    return window

        window = fixture.wait_for(main_window, 'private test window')
        window.set_input_focus(X.RevertToParent, X.CurrentTime)
        connection.sync()
        h.rpc(socket, 'pane_focus', pane=pane)

        def key(name, delay=.04, mods=()):
            # Xlib can choose the ISO <> key for XK_less; Shift then means >.
            # Use the US physical comma/period keys for these shifted tests.
            name = {'less': 'comma', 'greater': 'period'}.get(name, name)
            codes = [connection.keysym_to_keycode(XK.string_to_keysym(k)) for k in (*mods, name)]
            assert all(codes), (name, codes)
            for code in codes:
                xtest.fake_input(connection, X.KeyPress, code)
            for code in reversed(codes):
                xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()
            time.sleep(delay)

        def type_keys(text):
            for char in text:
                key(char.lower(), mods=('Shift_L',) if char.isupper() else ())

        def shot(name):
            root = connection.screen().root
            size = root.get_geometry()
            raw = root.get_image(0, 0, size.width, size.height, X.ZPixmap, 0xffffffff)
            Image.frombytes('RGB', (size.width, size.height), raw.data, 'raw', 'BGRX').save(h.root / (name + '.png'))

        def received():
            records = [json.loads(line) for line in (h.root / 'input.jsonl').read_text().splitlines()]
            return b''.join(bytes.fromhex(record['hex']) for record in records)

        def case(name, action, expected):
            before = len(received())
            start = time.monotonic()
            action()
            time.sleep(.2)
            raw = received()[before:]
            clean = raw.replace(b'\x1b[I', b'').replace(b'\x1b[O', b'')
            item = dict(name=name, start=start, end=time.monotonic(), raw_hex=raw.hex(),
                        expected=expected, received=clean.decode('utf-8', 'replace'),
                        passed=clean == expected.encode(), focus_out=raw.count(b'\x1b[O'))
            cases.append(item)
            shot(name)
            print(json.dumps(item, ensure_ascii=False), flush=True)

        h.send(socket, pane, '/usr/bin/python3 ' + shlex.quote(str(sink)) + ' ' + shlex.quote(str(h.root / 'input.jsonl')))
        fixture.wait_for(lambda: 'INPUT READY' in h.screen(socket, pane), 'raw input sink')
        subprocess.run(['ibus', 'engine', 'xkb:us::eng'], env=h.env, check=True, capture_output=True, timeout=15)
        time.sleep(.4)
        case('english', lambda: type_keys('abc'), 'abc')
        case('empty_enter', lambda: key('Return'), '\r')
        case('fast_three_enter', lambda: [key('Return', .001) for _ in range(3)], '\r\r\r')
        subprocess.run(['ibus', 'engine', 'hangul'], env=h.env, check=True, capture_output=True, timeout=15)
        time.sleep(.4)
        key('space', mods=('Shift_L',))
        case('hidden_cursor_preedit', lambda: type_keys('rksk'), '가')
        case('enter_after_preedit', lambda: key('Return'), '나\r')
        case('sentence_enter', lambda: (type_keys('dkssudgktpdy'), key('Return')), '안녕하세요\r')
        case('backspace_composing', lambda: (type_keys('gks'), key('BackSpace')),
             '한\x7f' if args.nav == 'on' else '')
        case('symbol_after_backspace', lambda: key('question', mods=('Shift_L',)),
             '?' if args.nav == 'on' else '하?')
        case('shift_enter', lambda: (type_keys('rk'), key('Return', mods=('Shift_L',))), '가\x1b\r')
        case('space', lambda: (type_keys('gksrmf'), key('space')), '한글 ')
        case('number', lambda: (type_keys('dk'), key('1')), '아1')
        case('comma', lambda: (type_keys('dkssud'), key('comma')), '안녕,')
        for name, char in [('question', '?'), ('exclam', '!'), ('at', '@'), ('numbersign', '#'),
                           ('dollar', '$'), ('percent', '%'), ('asciicircum', '^'), ('ampersand', '&'),
                           ('asterisk', '*'), ('parenleft', '('), ('parenright', ')'), ('underscore', '_'),
                           ('plus', '+'), ('colon', ':'), ('quotedbl', '"'), ('less', '<'), ('greater', '>'),
                           ('braceleft', '{'), ('braceright', '}'), ('bar', '|'), ('asciitilde', '~')]:
            case('shift_symbol_' + name, lambda name=name: (type_keys('gks'), key(name, mods=('Shift_L',))), '한' + char)
        for name, typed, expected in [('double_consonants', 'RkEkQkTkWk', '까따빠싸짜'),
                                      ('compound_vowels', 'rhkehldnjdml', '과되워의'),
                                      ('final_cluster', 'rkqt', '값'), ('isolated_jamo', 'r', 'ㄱ')]:
            case(name, lambda typed=typed: (type_keys(typed), key('Return')), expected + '\r')
        case('keypad_enter', lambda: (type_keys('rk'), key('KP_Enter')), '가\r')
        case('hangul_english_toggle', lambda: (type_keys('gks'), key('space', mods=('Shift_L',)),
             type_keys('abc'), key('space', mods=('Shift_L',)), type_keys('rmf'), key('Return')), '한abc글\r')
        for name, encoded in [('Tab', '\t'), ('Left', '\x1b[D'), ('Right', '\x1b[C'),
                              ('Home', '\x1b[H'), ('End', '\x1b[F'), ('Delete', '\x1b[3~')]:
            case('composing_' + name, lambda name=name: (type_keys('rk'), key(name)), '가' + encoded)
            # End any preedit retained by a noncommitting key before the next case.
            key('Return')
            time.sleep(.1)
        case('shift_left_order', lambda: (type_keys('rk'), key('Left', mods=('Shift_L',)), key('Return')), '가\x1b[D\r')
        case('shift_right_order', lambda: (type_keys('rk'), key('Right', mods=('Shift_L',)), key('Return')), '가\x1b[C\r')
        case('escape_then_enter', lambda: (type_keys('rk'), key('Escape'), key('Return')), '가\x1b\r')
        result = dict(sync=args.sync, navigation_workaround=args.nav, backend='Xvfb/X11/private IBus',
                      cases=cases, passed=sum(c['passed'] for c in cases), failed=sum(not c['passed'] for c in cases),
                      note='Byte checks only; screenshots need visual review. Not a WSLg portal or Windows test.')
        (h.root / 'result.json').write_text(json.dumps(result, ensure_ascii=False, indent=2) + '\n')
        h.close_window(app)
        return 1 if result['failed'] else 0
    finally:
        if connection is not None:
            connection.close()
        for child in reversed(h.children):
            h.stop(child)
        h.events.close()


if __name__ == '__main__':
    raise SystemExit(main())
