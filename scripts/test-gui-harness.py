#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Check GUI fixture readiness and deadlines without launching a desktop."""
from contextlib import contextmanager
import importlib.util
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
