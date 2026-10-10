#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify restored web tabs stay unloaded until mapped or explicitly addressed."""
import argparse
import importlib.util
import json
from pathlib import Path
import sys
import time
import uuid

from Xlib import X, XK, display
from Xlib.ext import xtest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "gui", Path(__file__).with_name("test-ssh-workspace-gui.py"))
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


def descendants(pid):
    parents = {}
    for path in Path('/proc').iterdir():
        if path.name.isdigit():
            try:
                fields = (path / 'stat').read_text().rsplit(')', 1)[1].split()
                parents[int(path.name)] = int(fields[1])
            except (OSError, ValueError):
                pass
    found = {pid}
    while True:
        children = {child for child, parent in parents.items() if parent in found}
        if children <= found:
            return found
        found |= children


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default='target/debug/flowmux')
    parser.add_argument('--cli', default='target/debug/flowmuxctl')
    parser.add_argument('--protected-pid', type=int, action='append', default=[])
    args = parser.parse_args()
    args.gui = str(Path(args.gui).resolve(strict=True))
    args.cli = str(Path(args.cli).resolve(strict=True))
    h = gui.Harness(args)
    connection = None
    try:
        shell = h.root / 'clean-shell'
        shell.write_text('#!/bin/sh\nexec /bin/bash --noprofile --norc\n')
        shell.chmod(0o755)
        config = h.root / 'config/flowmux'
        config.mkdir()
        (config / 'options.json').write_text(json.dumps({'default_shell': str(shell)}))
        project = h.root / 'project'
        project.mkdir()
        document = project / 'sample.txt'
        document.write_text('restored editor text\n')
        page = project / 'fixture.html'
        page.write_text('<title>Lazy fixture</title><p>Ready</p>')
        h.start_display()
        process, socket = h.window('seed')
        terminal_ws = h.rpc(socket, 'workspace_create', name='Terminal', root=str(project))['workspace_created']['id']
        hidden_ws = h.rpc(socket, 'workspace_create', name='Deferred web tabs', root=str(project))['workspace_created']['id']
        h.rpc(socket, 'workspace_focus', workspace=terminal_ws)
        pane = h.workspace(socket, terminal_ws)['panes'][0]['id']
        h.send(socket, pane, "printf 'SCROLLBACK_RESTORE_MARKER\\n'")
        gui.wait_for(lambda: 'SCROLLBACK_RESTORE_MARKER' in h.screen(socket, pane), 'terminal output')
        h.close_window(process)
        saved = json.loads(h.state_path.read_text())
        hidden = next(w for w in saved['workspaces'] if w['id'] == hidden_ws)
        leaf = hidden['surfaces'][0]['root_pane']
        browser_id, editor_id, unused_id = [str(uuid.uuid4()) for _ in range(3)]
        browser = {'id': browser_id, 'title': 'Lazy browser', 'kind': {
            'type': 'browser', 'initial_url': page.as_uri()}}
        editor = {'id': editor_id, 'title': 'sample.txt', 'kind': {
            'type': 'editor', 'workspace_root': str(project), 'session': {
                'open_files': [{'path': str(document)}], 'active_file': str(document)}}}
        unused = {**browser, 'id': unused_id, 'title': 'Never opened'}
        leaf['content'] = {'type': 'tabs', 'active': browser_id, 'surfaces': [browser, editor, unused]}
        h.state_path.write_text(json.dumps(saved))
        process, socket = h.window('restored')
        time.sleep(2)
        assert 'SCROLLBACK_RESTORE_MARKER' in h.screen(socket, pane)
        names = []
        for pid in descendants(process.pid):
            try:
                names.append(Path(f'/proc/{pid}/comm').read_text().strip())
            except OSError:
                pass
        assert not any('WebKit' in name for name in names), names
        h.pass_check('restored hidden browser and editor tabs do not start WebKit; terminal history survives')
        h.close_window(process)
        unopened = json.loads(h.state_path.read_text())
        preserved = next(w for w in unopened['workspaces'] if w['id'] == hidden_ws)
        tabs = preserved['surfaces'][0]['root_pane']['content']['surfaces']
        assert next(t for t in tabs if t['id'] == browser_id)['kind']['initial_url'] == page.as_uri()
        assert next(t for t in tabs if t['id'] == editor_id)['kind']['session']['active_file'] == str(document)
        process, socket = h.window('restored-again')
        h.pass_check('saving and restarting without visiting web tabs preserves their URLs and documents')
        hidden_pane = h.workspace(socket, hidden_ws)['panes'][0]['id']
        gui.wait_for(lambda: h.rpc(socket, 'browser_title', pane=hidden_pane)['browser_result']['value'] == 'Lazy fixture',
                     'IPC materializes hidden browser')
        h.pass_check('browser IPC initializes an unvisited restored tab')
        h.rpc(socket, 'surface_close', pane=hidden_pane, surface=unused_id)
        h.rpc(socket, 'workspace_focus', workspace=hidden_ws)
        h.rpc(socket, 'surface_focus', pane=hidden_pane, surface=editor_id)
        connection = display.Display(h.env['DISPLAY'])
        for window in connection.screen().root.query_tree().children:
            pid = window.get_full_property(connection.intern_atom('_NET_WM_PID'), X.AnyPropertyType)
            if pid is not None and int(pid.value[0]) == process.pid and window.get_attributes().map_state == X.IsViewable:
                window.set_input_focus(X.RevertToParent, X.CurrentTime)
        connection.sync()
        # WebKit loads the saved document asynchronously; disk contents below
        # verify native input reached the restored editor rather than a shell.
        time.sleep(3)
        def keys(*names):
            codes = [connection.keysym_to_keycode(XK.string_to_keysym(n)) for n in names]
            assert all(codes)
            for code in codes:
                xtest.fake_input(connection, X.KeyPress, code)
            for code in reversed(codes):
                xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()
            time.sleep(.15)
        keys('Control_L', 'End')
        keys('x')
        keys('Control_L', 's')
        gui.wait_for(lambda: document.read_text() == 'restored editor text\nx', 'restored editor edits and saves')
        h.pass_check('selected editor restores its document and saves native edits; unopened tab closes safely')
        h.close_window(process)
    finally:
        if connection:
            connection.close()
        h.cleanup()


if __name__ == '__main__':
    main()
