#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run SSH fixture readiness regressions without an installed SSH server."""

import argparse
import importlib.util
from pathlib import Path
import socket
import sys
import tempfile
import threading
import unittest
from unittest.mock import Mock, patch

sys.dont_write_bytecode = True

spec = importlib.util.spec_from_file_location(
    "fixture", Path(__file__).with_name("ssh-workspace-fixture.py")
)
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class SshdReadinessTest(unittest.TestCase):
    def test_tool_check_rejects_missing_shim_and_unloadable_programs(self):
        args = argparse.Namespace(sshd="sshd", tmux="tmux")
        for version, status in (("tmux 3.4 (flowmux tmux-compat shim)", 0),
                                ("tmux 3.4", 127)):
            with self.subTest(version=version, status=status), \
                 patch.object(fixture.subprocess, "run", side_effect=[
                     Mock(returncode=0, stdout="", stderr="OpenSSH_9.6"),
                     Mock(returncode=status, stdout=version, stderr=""),
                 ]):
                with self.assertRaisesRegex(RuntimeError, "not the real tmux"):
                    fixture.check_tools(args, {})
        args.sshd = None
        with self.assertRaisesRegex(RuntimeError, "SSH_FIXTURE_SSHD"):
            fixture.check_tools(args, {})

    def test_check_uses_configured_tools_and_library_path_without_starting_server(self):
        env = {"SSH_FIXTURE_SSHD": "/tools/sshd", "SSH_FIXTURE_TMUX": "/tools/tmux",
               "SSH_FIXTURE_LIBRARY_PATH": "/tools/lib"}
        with patch.dict(fixture.os.environ, env), \
             patch.object(sys, "argv", ["fixture", "--check"]), \
             patch.object(fixture.subprocess, "run", side_effect=[
                 Mock(returncode=0, stdout="", stderr="OpenSSH_9.6"),
                 Mock(returncode=0, stdout="tmux 3.4", stderr=""),
             ]) as run:
            fixture.main()
        self.assertEqual([call.args[0] for call in run.call_args_list],
                         [["/tools/sshd", "-V"], ["/tools/tmux", "-V"]])
        self.assertEqual(run.call_args.kwargs["env"]["LD_LIBRARY_PATH"], "/tools/lib")

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
