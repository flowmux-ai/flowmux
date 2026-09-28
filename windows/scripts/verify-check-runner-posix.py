#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Cancel a POSIX check runner and verify its owned descendants are gone."""
import argparse
import ctypes
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import uuid


SCRIPT = Path(__file__).resolve()
RUNNER = SCRIPT.with_name('run-check.py')
CHECKS = SCRIPT.parent.parent / 'dist' / 'checks'


def fixture(role, ready):
    if role == '--fixture-root':
        subprocess.Popen([sys.executable, str(SCRIPT), '--fixture-child', ready])
    else:
        grandchild = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        Path(ready).write_text(json.dumps(dict(root=os.getppid(), child=os.getpid(), grandchild=grandchild.pid)))
    time.sleep(30)


def existing(pids):
    remaining = []
    for pid in pids:
        # Linux subreaper mode lets this test reap orphaned descendants itself,
        # including on systems whose PID 1 does not promptly collect zombies.
        try:
            os.waitpid(pid, os.WNOHANG)
        except ChildProcessError:
            pass
        try:
            os.kill(pid, 0)
            remaining.append(pid)
        except ProcessLookupError:
            pass
    return remaining


def run_case(signum):
    name = 'cancel-' + signal.Signals(signum).name.lower() + '-' + str(uuid.uuid4())
    owned = {}
    with tempfile.TemporaryDirectory(prefix='flowmux-cancel-') as directory:
        ready = Path(directory) / 'ready.json'
        with (Path(directory) / 'runner.log').open('w+') as output:
            runner = subprocess.Popen(
                [sys.executable, str(RUNNER), '--name', name, '--timeout-seconds', '20', '--',
                 sys.executable, str(SCRIPT), '--fixture-root', str(ready)],
                stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
            try:
                deadline = time.monotonic() + 5
                while not owned:
                    if runner.poll() is not None or time.monotonic() >= deadline:
                        raise AssertionError('Fixture did not become ready within five seconds')
                    try:
                        owned = json.loads(ready.read_text())
                    except (FileNotFoundError, json.JSONDecodeError):
                        time.sleep(0.01)
                assert all(os.getpgid(pid) == owned['root'] for pid in owned.values()), owned
                started = time.monotonic()
                runner.send_signal(signum)
                code = runner.wait(timeout=5)
                elapsed = time.monotonic() - started
                paths = list(CHECKS.glob(name + '-*/result.json'))
                assert len(paths) == 1, paths
                result = json.loads(paths[0].read_text())
                assert code == 128 + signum, (code, result)
                assert result['status'] == 'cancelled', result
                assert result['exitCode'] == code and result['signal'] == signal.Signals(signum).name, result
                deadline = time.monotonic() + 3
                while remaining := existing(owned.values()):
                    if time.monotonic() >= deadline:
                        raise AssertionError('Owned processes survived cancellation: ' + str(remaining))
                    time.sleep(0.01)
                return dict(signal=signal.Signals(signum).name, runnerPid=runner.pid, ownedPids=owned,
                            exitCode=code, cancellationSeconds=round(elapsed, 3),
                            activeAfterCleanup=0, runnerResult=result)
            finally:
                # Limit failure cleanup to the exact fixture group we created.
                if not owned and ready.exists():
                    owned = json.loads(ready.read_text())
                if owned:
                    try:
                        os.killpg(owned['root'], signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                if runner.poll() is None:
                    runner.terminate()
                    try:
                        runner.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        runner.kill()
                        runner.wait(timeout=3)
                existing(owned.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    if os.name != 'posix':
        parser.error('POSIX processes only; do not run this script on Windows')
    subreaper = sys.platform.startswith('linux')
    if subreaper:
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
            raise OSError(ctypes.get_errno(), 'Could not enable fixture descendant reaping')
    checks = [run_case(signum) for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT)]
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(dict(status='passed', linuxSubreaper=subreaper, checks=checks), indent=2) + '\n')
    print('POSIX cancellation passed: SIGTERM=143, SIGHUP=129, SIGINT=130; no owned processes remain')
    print('Evidence: ' + str(args.output))


if __name__ == '__main__':
    if len(sys.argv) == 3 and sys.argv[1] in ('--fixture-root', '--fixture-child'):
        fixture(sys.argv[1], sys.argv[2])
    else:
        main()
