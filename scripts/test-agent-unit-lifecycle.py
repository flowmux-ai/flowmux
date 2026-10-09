#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify runner cancellation against a real systemd user manager (Linux only)."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time

RUNNER = Path(__file__).with_name('test-agent-unit.py').resolve()


def wait_for(predicate, message, timeout=20):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(.05)
    raise AssertionError(message)


def main():
    if sys.platform != 'linux':
        raise SystemExit('This check requires a Linux systemd user manager')
    with tempfile.TemporaryDirectory(prefix='fm-unit-lifecycle-') as directory:
        root = Path(directory)
        cargo = root / 'cargo'
        marker = root / 'started.json'
        ready = root / 'child-ready'
        child_code = ("import signal,time,pathlib; "
                      "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
                      f"pathlib.Path({str(ready)!r}).touch(); time.sleep(120)")
        cargo.write_text(f'''#!{sys.executable}
import json,os,pathlib,subprocess,sys,time
child = subprocess.Popen([sys.executable, '-c', {child_code!r}])
while not pathlib.Path({str(ready)!r}).exists(): time.sleep(.01)
record = pathlib.Path({str(marker.with_suffix('.tmp'))!r})
record.write_text(json.dumps({{
    'pids': [os.getpid(), child.pid],
    'cgroup': pathlib.Path('/proc/self/cgroup').read_text(),
    'target': os.environ.get('CARGO_TARGET_DIR'),
    'debug': os.environ.get('CARGO_PROFILE_TEST_DEBUG'),
    'socket': os.environ.get('FLOWMUX_SOCKET_PATH'),
    'runtime': os.environ['FLOWMUX_RUNTIME_DIR'],
}}))
record.replace({str(marker)!r})
time.sleep(120)
''')
        cargo.chmod(0o755)
        env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ['PATH'],
                   CARGO_TARGET_DIR=str(root / 'build cache'), CARGO_PROFILE_TEST_DEBUG='0',
                   FLOWMUX_SOCKET_PATH='/must/not/use.sock')
        for sig in (signal.SIGINT, signal.SIGTERM):
            marker.unlink(missing_ok=True)
            ready.unlink(missing_ok=True)
            with (root / 'runner.log').open('w+') as log:
                runner = subprocess.Popen([sys.executable, str(RUNNER)], env=env,
                                          stdout=log, stderr=subprocess.STDOUT)
                try:
                    wait_for(lambda: marker.exists() or runner.poll() is not None, 'worker start')
                    if not marker.exists():
                        log.seek(0)
                        raise AssertionError(log.read())
                    state = json.loads(marker.read_text())
                    assert state['target'] == env['CARGO_TARGET_DIR']
                    assert state['debug'] == '0' and state['socket'] is None
                    assert Path(state['runtime']).parent != root
                    unit = state['cgroup'].strip().split('/')[-1]
                    assert unit.startswith('flowmux-unit-') and unit.endswith('.service')
                    runner.send_signal(sig)
                    assert runner.wait(timeout=20) == 128 + sig
                    wait_for(lambda: all(not Path(f'/proc/{pid}').exists() for pid in state['pids']),
                             'Cargo or its SIGTERM-resistant child survived cancellation')
                    status = subprocess.check_output(
                        ['systemctl', '--user', 'show', unit, '-p', 'ActiveState', '--value'], text=True)
                    assert status.strip() == 'inactive', status
                    assert not Path(state['runtime']).parent.exists(), 'worker state directory leaked'
                    print(f'PASS: {sig.name}: build environment forwarded; service and descendants removed')
                finally:
                    if runner.poll() is None:
                        runner.terminate()
                        runner.wait(timeout=20)


if __name__ == '__main__':
    main()
