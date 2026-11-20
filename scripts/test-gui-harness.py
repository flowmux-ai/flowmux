#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check GUI fixture readiness and deadlines without launching a desktop."""
from contextlib import contextmanager
import importlib.util
import io
import json
from pathlib import Path
import socket
import sys
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('gui', Path(__file__).with_name('test-ssh-workspace-gui.py'))
gui = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gui)


class HarnessTests(unittest.TestCase):
    def test_window_waits_for_visible_x11_window_after_socket_ping(self):
        h = gui.Harness(SimpleNamespace(cli='/bin/true', gui='/bin/false', protected_pid=[]))
        h.env['DISPLAY'] = ':42'
        process = Mock(pid=123, poll=Mock(return_value=None))
        window = Mock()
        window.get_full_property.return_value.value = [123]
        window.get_attributes.side_effect = [SimpleNamespace(map_state=state) for state in
                                             [gui.X.IsUnmapped, gui.X.IsUnviewable, gui.X.IsViewable]]
        connection = Mock()
        connection.screen.return_value.root.query_tree.return_value.children = [window]
        try:
            with self.clock(), patch.object(h, 'spawn', return_value=process), \
                 patch.object(h, 'rpc', return_value=True), \
                 patch.object(Path, 'exists', return_value=True), \
                 patch.object(gui.display, 'Display', return_value=connection):
                self.assertIs(h.window('pending')[0], process)
            self.assertEqual(window.get_attributes.call_count, 3)
        finally:
            h.cleanup()

    def test_close_without_owned_window_fails_before_process_wait(self):
        h = gui.Harness(SimpleNamespace(cli='/bin/true', gui='/bin/false', protected_pid=[]))
        h.env['DISPLAY'] = ':42'
        process = Mock(pid=123, args=['/bin/false'], returncode=0)
        process.poll.return_value = 0
        h.children.append(process)
        window = Mock()
        window.get_full_property.return_value.value = [456]
        connection = Mock()
        connection.screen.return_value.root.query_tree.return_value.children = [window]
        try:
            with patch.object(gui.display, 'Display', return_value=connection):
                with self.assertRaisesRegex(AssertionError, 'No X11 window for GUI 123'):
                    h.close_window(process)
            window.send_event.assert_not_called()
            process.wait.assert_not_called()
        finally:
            h.cleanup()

    def test_close_timeout_survives_diagnostic_write_failure(self):
        h = gui.Harness(SimpleNamespace(cli='/bin/true', gui='/bin/false', protected_pid=[]))
        h.env['DISPLAY'] = ':42'
        timeout = gui.subprocess.TimeoutExpired(['/bin/false'], 20)
        process = Mock(pid=123456789, args=['/bin/false'], returncode=None)
        process.wait.side_effect = timeout
        process.poll.return_value = 0
        h.children.append(process)
        connection = Mock()
        connection.intern_atom.return_value = 1
        window = Mock(id=42)
        window.__window__ = lambda: 42
        window.get_full_property.return_value.value = [process.pid]
        window.get_attributes.return_value.map_state = 2
        connection.screen.return_value.root.query_tree.return_value.children = [window]
        try:
            with patch.object(gui.display, 'Display', return_value=connection), \
                 patch.object(Path, 'write_text', side_effect=OSError('disk full')), \
                 patch.object(gui.sys, 'stderr', new_callable=io.StringIO) as stderr:
                with self.assertRaises(gui.subprocess.TimeoutExpired) as error:
                    h.close_window(process)
            self.assertIs(error.exception, timeout)
            self.assertIn('Could not save close-timeout diagnostics: disk full', stderr.getvalue())
            process.wait.assert_called_once_with(timeout=20)
            process.terminate.assert_not_called()
            process.kill.assert_not_called()
        finally:
            h.cleanup()

    def test_close_timeout_captures_windows_before_cleanup_without_retrying(self):
        h = gui.Harness(SimpleNamespace(cli='/bin/true', gui='/bin/false', protected_pid=[]))
        h.env['DISPLAY'] = ':42'
        timeout = gui.subprocess.TimeoutExpired(['/bin/false'], 20)
        process = Mock(pid=123456789, args=['/bin/false'], returncode=None)
        process.wait.side_effect = timeout
        process.poll.return_value = 0
        h.children.append(process)
        window = Mock(id=42)
        window.__window__ = lambda: 42
        window.get_wm_name.return_value = 'Flowmux'
        window.get_attributes.return_value.map_state = 2
        window.get_full_property.return_value.value = [process.pid]
        connection = Mock()
        connection.intern_atom.return_value = 1
        connection.screen.return_value.root.query_tree.return_value.children = [window]
        try:
            with patch.object(gui.display, 'Display', return_value=connection):
                with self.assertRaises(gui.subprocess.TimeoutExpired) as error:
                    h.close_window(process)
            self.assertIs(error.exception, timeout)
            record = json.loads((h.root / f'close-timeout-{process.pid}.json').read_text())
            self.assertEqual(record['windows'], [dict(id=42, name='Flowmux', mapped=2)])
            process.wait.assert_called_once_with(timeout=20)
            process.terminate.assert_not_called()
            process.kill.assert_not_called()
        finally:
            h.cleanup()

    def test_session_click_waits_for_allocated_and_settled_popup_bounds(self):
        spec = importlib.util.spec_from_file_location(
            'session_resume', Path(__file__).with_name('test-agent-session-resume-gui.py'))
        session = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(session)
        rectangles = iter([(0, 0, 0, 0), (0, 0, 0, 0), (0, 0, 34, 10),
                           (60, 90, 180, 36), (60, 90, 180, 36)])
        component = Mock()
        component.get_extents.side_effect = lambda _: SimpleNamespace(
            **dict(zip(('x', 'y', 'width', 'height'), next(rectangles))))
        node = Mock()
        node.get_component_iface.return_value = component
        find = Mock(return_value=node)
        with self.clock():
            target, bounds = session.click_target(find, 'button', 'New Agent…', 'window')
        self.assertIs(target, node)
        self.assertEqual((bounds.x, bounds.y, bounds.width, bounds.height), (60, 90, 180, 36))
        self.assertEqual(find.call_count, 5)

    def test_accessibility_driver_uses_isolated_display_and_bus(self):
        stale = dict(DISPLAY=':999', DBUS_SESSION_BUS_ADDRESS='unix:path=/outer',
                     AT_SPI_BUS_ADDRESS='unix:path=/outer-a11y', AT_SPI_DISPLAY=':998',
                     WAYLAND_DISPLAY='outer-wayland')
        with patch.dict(gui.os.environ, stale):
            h = gui.Harness(SimpleNamespace(cli='/bin/true', gui='/bin/false', protected_pid=[]))
            h.env['GTK_A11Y'] = 'atspi'
            def spawn(argv, **kwargs):
                if argv[0] == 'Xvfb':
                    gui.os.write(kwargs['pass_fds'][0], b'42\n')
                return Mock(stdout=io.BytesIO(b'unix:path=/private\n'))
            try:
                with patch.object(h, 'spawn', side_effect=spawn), \
                     patch.object(gui.select, 'select', return_value=([1], [], [])):
                    h.start_display()
                for variable, expected in [('DISPLAY', ':42'),
                                           ('DBUS_SESSION_BUS_ADDRESS', 'unix:path=/private')]:
                    self.assertEqual(h.env[variable], expected)
                    self.assertEqual(gui.os.environ[variable], expected)
                for variable in ('AT_SPI_BUS_ADDRESS', 'AT_SPI_DISPLAY', 'WAYLAND_DISPLAY'):
                    self.assertNotIn(variable, h.env)
                    self.assertNotIn(variable, gui.os.environ)
            finally:
                h.cleanup()

    def test_isolated_gui_disables_user_skill_refresh(self):
        h = gui.Harness(SimpleNamespace(cli='/bin/true', gui='/bin/false', protected_pid=[]))
        try:
            marker = Path(h.env['XDG_STATE_HOME']) / 'flowmux/installed-skill-version'
            self.assertEqual(marker.read_text(), '9999.0.0')
            self.assertEqual(h.env['HOME'], gui.os.environ['HOME'])
            self.assertTrue(marker.is_relative_to(h.root))
        finally:
            h.cleanup()

    @contextmanager
    def clock(self):
        now = [0.0]
        def advance(seconds):
            now[0] += seconds
        with patch.object(gui.time, 'monotonic', side_effect=lambda: now[0]), \
             patch.object(gui.time, 'sleep', side_effect=advance):
            yield now, advance

    def test_late_success_is_not_readiness(self):
        with self.clock() as (_, advance):
            def late():
                advance(2)
                return True
            with self.assertRaisesRegex(AssertionError, 'late result'):
                gui.wait_for(late, 'late result', timeout=1)

    def test_nested_waits_share_deadline_and_restore_it(self):
        with self.clock() as (now, _):
            with self.assertRaisesRegex(AssertionError, 'inner'):
                gui.wait_for(lambda: gui.wait_for(lambda: False, 'inner', timeout=30),
                             'outer', timeout=1)
            self.assertAlmostEqual(now[0], 1)
            self.assertTrue(gui.wait_for(lambda: True, 'next independent wait', timeout=1))

    def test_short_inner_wait_does_not_extend_outer_deadline(self):
        with self.clock() as (now, _):
            def check():
                try:
                    gui.wait_for(lambda: False, 'inner', timeout=.2)
                except AssertionError:
                    return False
            with self.assertRaisesRegex(AssertionError, 'outer'):
                gui.wait_for(check, 'outer', timeout=1)
            self.assertAlmostEqual(now[0], 1)

    def test_silent_ipc_cannot_outlive_readiness_budget(self):
        for trickle in (False, True):
            with self.subTest(trickle=trickle):
                h = gui.Harness(SimpleNamespace(cli='/bin/true', gui='/bin/false', protected_pid=[]))
                release = threading.Event()
                path = h.root / 'run/silent.sock'
                with socket.socket(socket.AF_UNIX) as server:
                    server.settimeout(3)
                    server.bind(str(path))
                    server.listen()
                    def serve():
                        with server.accept()[0] as stream:
                            if not trickle:
                                release.wait(2)
                                return
                            # Continual partial data must not reset the response deadline.
                            while not release.wait(.01):
                                try:
                                    stream.sendall(b' ')
                                except OSError:
                                    return
                    thread = threading.Thread(target=serve)
                    thread.start()
                    try:
                        started = time.monotonic()
                        with self.assertRaisesRegex(AssertionError, 'IPC readiness'):
                            gui.wait_for(lambda: h.rpc(path, 'ping'), 'IPC readiness', timeout=.1)
                        self.assertLess(time.monotonic() - started, 1)
                    finally:
                        release.set()
                        thread.join(timeout=4)
                        h.cleanup()

    def test_exited_gui_fails_before_waiting_for_socket(self):
        h = gui.Harness(SimpleNamespace(cli='/bin/true', gui='/bin/false', protected_pid=[]))
        process = Mock(pid=123, poll=Mock(return_value=17))
        try:
            with self.clock() as (now, _), patch.object(h, 'spawn', return_value=process):
                with self.assertRaisesRegex(AssertionError, 'exited.*17'):
                    h.window('crashed')
                self.assertEqual(now[0], 0)
        finally:
            h.cleanup()


if __name__ == '__main__':
    unittest.main()
