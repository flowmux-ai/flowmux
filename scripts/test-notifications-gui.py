#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Exercise notification controls in an isolated Linux Flowmux GUI.

Requires the Xvfb/D-Bus Harness dependencies plus python3-gi,
gir1.2-atspi-2.0 and python3-pil. Screenshots and IPC logs are retained.
Use --self-check to check syntax without launching a GUI or importing GI.
"""
import argparse
from contextlib import closing
import importlib.util
import json
import os
from pathlib import Path
import sys
import time

sys.dont_write_bytecode = True


def exercise(h, fixture, Atspi, display, X, xtest, Image):
    process, socket = h.window('notifications')

    def rpc(verb, **fields):
        return h.rpc(socket, verb, **fields)

    def snapshot():
        return rpc('notifications_list', unread_only=False)['notifications']

    def state(total, unread):
        def matches():
            value = snapshot()
            return value if len(value['entries']) == total and value['unread_count'] == unread else None
        return fixture.wait_for(matches, f'{total} retained notifications, {unread} unread')

    def walk(node):
        yield node
        for index in range(node.get_child_count()):
            child = node.get_child_at_index(index)
            if child:
                yield from walk(child)

    def visible(node):
        return node.get_state_set().contains(Atspi.StateType.SHOWING)

    def named(name, parent=None, role=None):
        return next((node for node in walk(parent or Atspi.get_desktop(0))
                     if visible(node) and name in (node.get_name(), node.get_description())
                     and (role is None or node.get_role() == role)), None)

    def find(name, parent=None, role=None):
        h.log('find', name=name)
        try:
            return fixture.wait_for(lambda: named(name, parent, role), name)
        except AssertionError:
            (h.root / 'accessibility.txt').write_text('\n'.join(
                f'{node.get_role_name()} showing={visible(node)} name={node.get_name()!r} description={node.get_description()!r}'
                for node in walk(Atspi.get_desktop(0))))
            screenshot('notifications-failure.png')
            raise

    def click(node, pointer=False):
        assert node.get_state_set().contains(Atspi.StateType.SENSITIVE), node.get_name()
        action = node.get_action_iface()
        if not pointer and action and action.get_n_actions():
            assert action.do_action(0), node.get_name()
        else:
            time.sleep(.3)
            bounds = node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
            assert bounds.width > 0 and bounds.height > 0, node.get_name()
            with closing(display.Display(h.env['DISPLAY'])) as connection:
                click_x, click_y = bounds.x + bounds.width // 2, bounds.y + bounds.height // 2
                # GTK's AT-SPI screen origin can be unavailable on Xvfb.
                if pointer and bounds.x == bounds.y == 0:
                    bounds = node.get_component_iface().get_extents(Atspi.CoordType.WINDOW)
                    atom = connection.intern_atom('_NET_WM_PID')
                    native = next(window for window in connection.screen().root.query_tree().children
                                  if not window.get_attributes().override_redirect
                                  and (pid := window.get_full_property(atom, X.AnyPropertyType)) is not None
                                  and int(pid.value[0]) == process.pid and window.get_geometry().width > 100)
                    origin = native.get_geometry()
                    click_x = origin.x + bounds.x + bounds.width // 2
                    click_y = origin.y + bounds.y + bounds.height // 2
                h.log('pointer-click', name=node.get_name(), x=click_x, y=click_y)
                xtest.fake_input(connection, X.MotionNotify,
                                 x=click_x, y=click_y)
                xtest.fake_input(connection, X.ButtonPress, 1)
                xtest.fake_input(connection, X.ButtonRelease, 1)
                connection.sync()

    def key(*names):
        from Xlib import XK
        with closing(display.Display(h.env['DISPLAY'])) as connection:
            codes = [connection.keysym_to_keycode(XK.string_to_keysym(name)) for name in names]
            assert all(codes), names
            for code in codes:
                xtest.fake_input(connection, X.KeyPress, code)
            for code in reversed(codes):
                xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()

    def screenshot(name):
        with closing(display.Display(h.env['DISPLAY'])) as connection:
            xtest.fake_input(connection, X.MotionNotify, x=10, y=10)
            connection.sync()
            time.sleep(.2)
            root = connection.screen().root
            size = root.get_geometry()
            raw = root.get_image(0, 0, size.width, size.height, X.ZPixmap, 0xFFFFFFFF)
            Image.frombytes('RGB', (size.width, size.height), raw.data, 'raw', 'BGRX').save(h.root / name)

    def ancestor(node, role):
        while node and node.get_role() != role:
            node = node.get_parent()
        assert node, f'Missing accessible ancestor {role}'
        return node

    def popup(unread):
        heading = find(f'Notifications · {unread} unread')
        # The popover's root box contains the heading, filters, dropdown and list.
        return heading.get_parent().get_parent()

    def open_bell(unread):
        click(find(f'Notifications ({unread} unread)', role=Atspi.Role.TOGGLE_BUTTON))
        return popup(unread)

    def counts(root, values):
        for label, count in zip(('All', 'Unread'), values):
            find(f'{label} {count}', root)

    def rows(root, expected):
        def observed():
            titles = {node.get_name() for node in walk(root)
                      if visible(node) and node.get_name() in all_titles}
            return titles == set(expected)
        fixture.wait_for(observed, 'filtered rows: ' + ', '.join(expected))

    def select_filter(label, name):
        dropdown = find(label, role=Atspi.Role.COMBO_BOX)
        toggle = next(node for node in walk(dropdown) if node.get_role() == Atspi.Role.TOGGLE_BUTTON)
        click(toggle)
        label = find(name, dropdown, role=Atspi.Role.LABEL)
        item = ancestor(label, Atspi.Role.LIST_ITEM)
        key('Home')
        for _ in range(item.get_index_in_parent()):
            key('Down')
        key('Return')
        fixture.wait_for(lambda: dropdown.get_name() == name, 'filter selected: ' + name)

    def select_workspace(name):
        select_filter('Filter by workspace', name)

    def select_kind(name):
        select_filter('Filter by notification type', name)

    def current():
        return rpc('workspace_current')['workspace_current']['id']

    def active_tabs():
        return [(ws['id'], pane['id'], tab['id']) for ws in h.tree(socket)
                for pane in ws['panes'] for tab in pane['tabs'] if tab['active']]

    def notify(title, level, pane=None, surface=None):
        before = {entry['id'] for entry in snapshot()['entries']}
        rpc('notify', title=title, body='Regression detail for ' + title,
            level=level, pane=pane, surface=surface)
        entries = snapshot()['entries']
        fresh = [entry for entry in entries if entry['id'] not in before]
        assert len(fresh) == 1 and fresh[0]['title'] == title, entries
        return fresh[0]['id']

    root_a, root_b = h.root / 'Review A', h.root / 'Review B'
    root_a.mkdir()
    root_b.mkdir()
    a = rpc('workspace_create', name='Review A', root=str(root_a))['workspace_created']['id']
    pane_a = h.workspace(socket, a)['panes'][0]['id']
    source_tab = h.workspace(socket, a)['panes'][0]['tabs'][0]
    h.send(socket, pane_a, 'git init -q')
    history = h.root / 'history.py'
    history.write_text("for i in range(500): print('NOTIFICATION_HISTORY_%04d' % i)\n")
    h.send(socket, pane_a, f'/usr/bin/python3 {history}')
    fixture.wait_for(lambda: 'NOTIFICATION_HISTORY_0499' in h.screen(socket, pane_a), 'source output generated')
    from Xlib import XK
    with closing(display.Display(h.env['DISPLAY'])) as connection:
        atom = connection.intern_atom('_NET_WM_PID')
        window = next(window for window in connection.screen().root.query_tree().children
                      if (pid := window.get_full_property(atom, X.AnyPropertyType)) is not None
                      and int(pid.value[0]) == process.pid and window.get_geometry().width > 100)
        window.set_input_focus(X.RevertToParent, X.CurrentTime)
        connection.sync()
        for _ in range(4):
            codes = [connection.keysym_to_keycode(XK.string_to_keysym(k)) for k in ('Shift_L', 'Page_Up')]
            for code in codes:
                xtest.fake_input(connection, X.KeyPress, code)
            for code in reversed(codes):
                xtest.fake_input(connection, X.KeyRelease, code)
            connection.sync()
            time.sleep(.1)
    assert 'NOTIFICATION_HISTORY_0499' not in h.screen(socket, pane_a), 'Fixture must be scrolled away from the prompt'
    rpc('surface_create', workspace=a, cwd=str(root_a))
    b = rpc('workspace_create', name='Review B', root=str(root_b))['workspace_created']['id']
    pane_b = h.workspace(socket, b)['panes'][0]['id']
    tab_b = h.workspace(socket, b)['panes'][0]['tabs'][0]['id']
    observer = rpc('workspace_create', name='Observer', root=str(h.root))['workspace_created']['id']
    rpc('workspace_focus', workspace=observer)
    # Global attention messages and background sources avoid focus suppression.
    titles = ['Codex approval', 'Claude error', 'Gemini completed', 'Codex background error', 'Global input']
    all_titles = set(titles)
    ids = [notify(titles[0], 'needs_input', pane_a, source_tab['id']),
           notify(titles[1], 'error', pane_a, source_tab['id']),
           notify(titles[2], 'turn_completed', pane_a, source_tab['id']),
           notify(titles[3], 'error', pane_b, tab_b),
           notify(titles[4], 'needs_input')]
    state(5, 5)
    # An inset window exposes accidental clamping to the monitor instead of the app.
    with closing(display.Display(h.env['DISPLAY'])) as connection:
        atom = connection.intern_atom('_NET_WM_PID')
        window = next(window for window in connection.screen().root.query_tree().children
                      if (pid := window.get_full_property(atom, X.AnyPropertyType)) is not None
                      and int(pid.value[0]) == process.pid and window.get_geometry().width > 100)
        window.configure(x=180, y=100, width=1000, height=800)
        connection.sync()
    bell = find('Notifications (5 unread)', role=Atspi.Role.TOGGLE_BUTTON)
    frame = ancestor(bell, Atspi.Role.FRAME)
    root = open_bell(5)
    counts(root, (5, 5))
    # Measure mapped content rather than a requested size or CSS string.
    bounds = root.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
    assert bounds.width >= 580, bounds
    frame_bounds = frame.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
    bell_bounds = bell.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
    assert frame_bounds.x <= bounds.x and bounds.x + bounds.width <= frame_bounds.x + frame_bounds.width, bounds
    assert frame_bounds.y <= bounds.y and bounds.y + bounds.height <= frame_bounds.y + frame_bounds.height, bounds
    assert bounds.x >= bell_bounds.x - 12, (bounds, bell_bounds)
    scroll = next(node for node in walk(root) if node.get_role() == Atspi.Role.SCROLL_PANE)
    scroll_bounds = scroll.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
    assert 440 <= scroll_bounds.height <= 564, scroll_bounds  # GTK border allowance.
    row_bounds = [node.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
                  for node in walk(scroll) if visible(node) and node.get_role() == Atspi.Role.LIST_ITEM]
    assert len(row_bounds) == 5 and all(row.height <= 116 for row in row_bounds), row_bounds
    assert sum(scroll_bounds.y <= row.y and row.y + row.height <= scroll_bounds.y + scroll_bounds.height
               for row in row_bounds) >= 4, (scroll_bounds, row_bounds)
    state(5, 5)
    screenshot('notifications-open.png')
    key('Escape')
    fixture.wait_for(lambda: named('Notifications · 5 unread') is None, 'popover closed')
    root = open_bell(5)
    state(5, 5)
    h.pass_check('Popup opens inward from the bell inside an inset window; width, scroll and unread counts are preserved')

    select_kind('Input requests')
    rows(root, [titles[0], titles[4]])
    counts(root, (2, 2))
    select_kind('Errors')
    rows(root, [titles[1], titles[3]])
    select_kind('Completed')
    rows(root, [titles[2]])
    select_kind('All types')
    select_workspace('Review A')
    root = popup(5)
    counts(root, (3, 3))
    # Restrict to a kind before inspecting rows so offscreen rows are irrelevant.
    select_kind('Input requests')
    rows(root, [titles[0]])
    row = ancestor(find(titles[0], root), Atspi.Role.LIST_ITEM)
    source = f"Review A / {source_tab['title']} / Codex"
    find(source, row)
    find('Regression detail for ' + titles[0], row)
    icon = next(node for node in walk(row) if node.get_role() in (Atspi.Role.IMAGE, Atspi.Role.ICON)
                and 'codex' in (node.get_name().lower(), node.get_description().lower()))
    icon_bounds = icon.get_component_iface().get_extents(Atspi.CoordType.SCREEN)
    assert icon_bounds.width >= 28 and icon_bounds.height >= 28, icon_bounds
    screenshot('notifications-workspace-input.png')
    select_kind('Errors')
    rows(root, [titles[1]])
    select_kind('Completed')
    rows(root, [titles[2]])
    select_workspace('Review B')
    root = popup(5)
    counts(root, (0, 0))
    select_kind('Errors')
    counts(root, (1, 1))
    rows(root, [titles[3]])
    h.pass_check('Kind filters and counts scope to the selected workspace; source, body and agent icon are exposed')

    before = active_tabs()
    click(find('Delete notification', ancestor(find(titles[3], root), Atspi.Role.LIST_ITEM)))
    state(4, 4)
    assert current() == observer and active_tabs() == before, 'Delete navigated to the source'
    assert ids[3] not in {entry['id'] for entry in snapshot()['entries']}
    h.pass_check('Row deletion removes only its notification without workspace or tab navigation')

    select_workspace('Review A')
    root = popup(4)
    select_kind('Input requests')
    row = ancestor(find(titles[0], root), Atspi.Role.LIST_ITEM)
    open_button = find('Open source tab', row, Atspi.Role.PUSH_BUTTON)
    assert not any(tab['id'] == source_tab['id'] and tab['active']
                   for pane in h.workspace(socket, a)['panes'] for tab in pane['tabs']), \
        'Fixture must route to an inactive source tab'
    for _ in range(30):
        if open_button.get_state_set().contains(Atspi.StateType.FOCUSED):
            break
        key('Tab')
        time.sleep(.05)
    assert open_button.get_state_set().contains(Atspi.StateType.FOCUSED), 'Row must be reachable with Tab'
    key('Return')
    fixture.wait_for(lambda: current() == a, 'keyboard activation routes to source workspace')
    fixture.wait_for(lambda: any(tab['id'] == source_tab['id'] and tab['active']
                                for pane in h.workspace(socket, a)['panes'] for tab in pane['tabs']),
                     'keyboard activation routes to original source tab')
    fixture.wait_for(lambda: next(e for e in snapshot()['entries'] if e['id'] == ids[0])['read'],
                     'keyboard activation acknowledges source notification')
    fixture.wait_for(lambda: 'NOTIFICATION_HISTORY_0499' in h.screen(socket, pane_a),
                     'notification reveals the live prompt after source was scrolled back')
    screenshot('notifications-source-prompt.png')
    h.pass_check('Actual Return on the row opens its original workspace/tab, reveals the live prompt and acknowledges it')

    key('Control_L', 'Alt_L', 'e')
    find('Back to previous tab')
    root = open_bell(snapshot()['unread_count'])
    click(find('Open source tab', ancestor(find(titles[0], root), Atspi.Role.LIST_ITEM),
               Atspi.Role.PUSH_BUTTON), pointer=True)
    screenshot('notifications-review-click.png')
    fixture.wait_for(lambda: named('Back to previous tab') is None, 'notification leaves Code Review')
    fixture.wait_for(lambda: 'NOTIFICATION_HISTORY_0499' in h.screen(socket, pane_a), 'source prompt restored from review')
    key('z')
    fixture.wait_for(lambda: h.screen(socket, pane_a).rstrip().endswith('z'), 'keyboard focus returns to source prompt')
    key('BackSpace')
    h.pass_check('Mouse click on a notification exits Code Review and returns to the source terminal')

    rpc('workspace_focus', workspace=observer)
    unread = snapshot()['unread_count']
    root = open_bell(unread)
    select_workspace('All workspaces')
    select_kind('All types')
    root = popup(unread)
    click(find('All 4', root))
    click(find('Mark all read', root, Atspi.Role.PUSH_BUTTON))
    state(4, 0)
    root = popup(0)
    counts(root, (4, 0))
    assert not find('Mark all read', root).get_state_set().contains(Atspi.StateType.SENSITIVE)
    click(find('Unread 0', root))
    find('No notifications match these filters.', root)
    screenshot('notifications-marked-read.png')
    h.pass_check('Mark all read acknowledges every workspace without deleting; Unread becomes empty')

    fresh = notify('Fresh global error', 'error')
    state(5, 1)
    root = popup(1)
    select_kind('Errors')
    counts(root, (2, 1))
    find('Fresh global error', root)
    assert named(titles[1], root) is None, 'Unread errors must exclude read errors'
    click(find('All 2', root))
    find(titles[1], root)
    click(find('Unread 1', root))
    select_kind('All types')
    h.pass_check('Read-state tabs combine with kind filters: unread errors exclude read errors')
    click(find('Notification actions', root, Atspi.Role.TOGGLE_BUTTON))
    click(find('Delete read notifications', role=Atspi.Role.PUSH_BUTTON))
    value = state(1, 1)
    assert value['entries'][0]['id'] == fresh, value
    root = popup(1)
    counts(root, (1, 1))
    find('Fresh global error', root)
    screenshot('notifications-delete-read.png')
    click(find('Notification actions', root, Atspi.Role.TOGGLE_BUTTON))
    click(find('Delete all notifications', role=Atspi.Role.PUSH_BUTTON))
    state(0, 0)
    root = popup(0)
    counts(root, (0, 0))
    find('No notifications yet.', root)
    screenshot('notifications-empty.png')
    h.pass_check('Delete read preserves fresh unread entries; Delete all clears the transcript and counts')
    # Target navigation must not acknowledge the tab that was selected before it.
    key('Escape')
    second_tab = next(tab['id'] for pane in h.workspace(socket, a)['panes']
                      for tab in pane['tabs'] if tab['id'] != source_tab['id'])
    rpc('surface_focus', pane=pane_a, surface=second_tab)
    rpc('workspace_focus', workspace=observer)
    target = notify('Target request', 'needs_input', pane_a, source_tab['id'])
    other = notify('Other tab request', 'needs_input', pane_a, second_tab)
    root = open_bell(2)
    click(find('All 2', root))
    click(find('Open source tab', ancestor(find('Target request', root), Atspi.Role.LIST_ITEM),
               Atspi.Role.PUSH_BUTTON), pointer=True)
    state(2, 1)
    assert not next(e for e in snapshot()['entries'] if e['id'] == other)['read']
    assert next(e for e in snapshot()['entries'] if e['id'] == target)['read']
    fixture.wait_for(lambda: current() == a, 'source workspace selected')
    rpc('workspace_focus', workspace=observer)
    rpc('surface_close', pane=pane_a, surface=source_tab['id'])
    rpc('workspace_focus', workspace=observer)
    unread = snapshot()['unread_count']
    root = open_bell(unread)
    click(find('Open source tab', ancestor(find('Target request', root), Atspi.Role.LIST_ITEM),
               Atspi.Role.PUSH_BUTTON))
    find('The source tab is no longer open.')
    assert current() == observer, 'Closed source must not switch to a fallback workspace'
    assert not next(e for e in snapshot()['entries'] if e['id'] == other)['read']
    h.pass_check('Actual mouse click opens the source and preserves other tabs; closed sources explain why they cannot open')
    # OSC first, completion hook second: refresh the existing row and its kind.
    info = notify('Terminal update', 'info', pane_b, tab_b)
    root = open_bell(snapshot()['unread_count'])
    select_kind('Information')
    counts(root, (1, 1))
    find('Terminal update', root)
    rpc('notify', title='Codex completed update', body='Ready for review',
        level='turn_completed', pane=pane_b, surface=tab_b)
    fixture.wait_for(lambda: next(e for e in snapshot()['entries'] if e['id'] == info)['level'] == 'turn_completed',
                     'existing notification promoted to completion')
    counts(root, (0, 0))
    select_kind('Completed')
    counts(root, (1, 1))
    find('Codex completed update', root)
    find('Ready for review', root)
    screenshot('notifications-promoted-completion.png')
    h.pass_check('Completion hook upgrades the earlier OSC row and refreshes its kind, title, body and filter counts')
    (h.root / 'final-notifications.json').write_text(json.dumps(snapshot(), indent=2))
    h.close_window(process)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--gui', default='target/debug/flowmux')
    parser.add_argument('--cli', default='target/debug/flowmuxctl')
    parser.add_argument('--protected-pid', type=int, action='append', default=[])
    parser.add_argument('--self-check', action='store_true')
    args = parser.parse_args()
    if args.self_check:
        compile(Path(__file__).read_text(), str(Path(__file__)), 'exec')
        print('PASS: notification GUI script compiles; live checks require the rebuilt GUI')
        return
    from PIL import Image
    from Xlib import X, display
    from Xlib.ext import xtest
    spec = importlib.util.spec_from_file_location('fixture', Path(__file__).with_name('test-ssh-workspace-gui.py'))
    fixture = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(fixture)
    args.gui = str(Path(args.gui).resolve(strict=True))
    args.cli = str(Path(args.cli).resolve(strict=True))
    h = fixture.Harness(args)
    h.env.update(GTK_A11Y='atspi', CLAUDE_CONFIG_DIR=str(h.root / 'claude'), CODEX_HOME=str(h.root / 'codex'))
    h.env.pop('AT_SPI_BUS_ADDRESS', None)
    # No user's shell startup files or agent hooks participate in this fixture.
    config = h.root / 'config/flowmux'
    config.mkdir()
    (config / 'options.json').write_text(json.dumps({'default_shell': '/bin/sh'}))
    try:
        h.start_display()
        os.environ['DISPLAY'] = h.env['DISPLAY']
        os.environ['DBUS_SESSION_BUS_ADDRESS'] = h.env['DBUS_SESSION_BUS_ADDRESS']
        os.environ.pop('AT_SPI_BUS_ADDRESS', None)
        import gi
        gi.require_version('Atspi', '2.0')
        from gi.repository import Atspi
        exercise(h, fixture, Atspi, display, X, xtest, Image)
    finally:
        h.cleanup()


if __name__ == '__main__':
    main()
