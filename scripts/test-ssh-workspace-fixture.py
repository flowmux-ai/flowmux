#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run SSH fixture readiness regressions without an installed SSH server."""

import importlib.util
from pathlib import Path
import socket
import sys
import tempfile
import threading
import unittest
from unittest.mock import Mock

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location(
    "fixture", Path(__file__).with_name("ssh-workspace-fixture.py")
)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class SshdReadinessTest(unittest.TestCase):
    def test_waits_for_delayed_listener(self):
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
            sshd = Mock()
            sshd.poll.return_value = None
            # Longer than the old fixed 0.2-second startup delay.
            timer = threading.Timer(0.4, listener.listen)
            timer.start()
            try:
                fixture.wait_for_sshd(sshd, port, Path("unused.log"))
                with socket.create_connection(("127.0.0.1", port), timeout=1):
                    pass
            finally:
                timer.cancel()
                timer.join()

    def test_reports_server_exit_without_waiting_for_timeout(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "sshd.log"
            log.write_text("invalid server configuration")
            sshd = Mock()
            sshd.poll.return_value = 1
            with self.assertRaisesRegex(RuntimeError, "invalid server configuration"):
                fixture.wait_for_sshd(sshd, 0, log)
            sshd.poll.assert_called_once()


if __name__ == "__main__":
    unittest.main()
