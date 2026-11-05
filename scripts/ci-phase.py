#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Bound one CI phase and retain its command, duration, and exit status."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def reap_detached_children():
    deadline = time.monotonic() + 5
    while True:
        try:
            while os.waitpid(-1, os.WNOHANG)[0]:
                pass
        except ChildProcessError:
            return
        # Subreaper adoption retains ownership even after setsid or double-fork.
        children = Path(f'/proc/self/task/{os.getpid()}/children').read_text().split()
        for child in children:
            try:
                os.kill(int(child), signal.SIGKILL)
            except ProcessLookupError:
                pass
        if time.monotonic() >= deadline:
            raise TimeoutError('detached test descendants did not exit')
        time.sleep(.01)


def interrupted(signum, _frame):
    raise SystemExit(128 + signum)


def main():
    directory, phase, budget, *command = sys.argv[1:]
    timeout = float(budget)
    if timeout <= 0 or not command:
        raise ValueError('a positive timeout and command are required')
    report = Path(directory) / (phase + '.json')
    report.parent.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    record = dict(phase=phase, command=command, timeout_seconds=timeout, status='running')
    report.write_text(json.dumps(record, indent=2) + '\n')
    print(f'CI_PHASE_START={phase} timeout={timeout}s command={command!r}', flush=True)
    process = None
    result, status = 1, 'error'
    for sig in (signal.SIGINT, signal.SIGTERM):
        signal.signal(sig, interrupted)
    try:
        if sys.platform == 'linux':
            import ctypes
            libc = ctypes.CDLL(None, use_errno=True)
            if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
                error = ctypes.get_errno()
                raise OSError(error, os.strerror(error))
        process = subprocess.Popen(command, start_new_session=True)
        record['pid'] = process.pid
        report.write_text(json.dumps(record, indent=2) + '\n')
        if sys.platform == 'linux':
            deadline = time.monotonic() + timeout
            while process.poll() is None:
                # Reap exited orphans during the suite so PID liveness checks stay valid.
                children = Path(f'/proc/self/task/{os.getpid()}/children').read_text().split()
                for child in map(int, children):
                    if child != process.pid:
                        os.waitpid(child, os.WNOHANG)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise subprocess.TimeoutExpired(command, timeout)
                time.sleep(min(.05, remaining))
        result = process.wait(timeout=timeout)
        if result < 0:
            result = 128 - result
        status = 'passed' if result == 0 else 'failed'
    except subprocess.TimeoutExpired:
        result, status = 124, 'timeout'
        if sys.platform == 'darwin':
            # GLib deadlines cannot fire when the native main thread is blocked.
            try:
                rows = subprocess.check_output(
                    ['ps', '-axo', 'pid=,pgid=,comm='], text=True, timeout=5)
                for row in rows.splitlines():
                    pid, group, executable = row.split(None, 2)
                    if int(group) == process.pid and Path(executable).name.startswith('macos_native-'):
                        subprocess.run(
                            ['/usr/bin/sample', pid, '3', '1', '-file',
                             str(report.with_suffix(f'.{pid}.sample'))], timeout=10, check=True)
            except (OSError, subprocess.SubprocessError) as error:
                record['diagnostic_error'] = str(error)
    except SystemExit as error:
        result, status = error.code, 'interrupted'
    except OSError as error:
        record['error'] = str(error)
    finally:
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, signal.SIG_IGN)
        if process is not None:
            # A completed parent can leave children holding CI output pipes open.
            try:
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                process.wait(timeout=5)
                if sys.platform == 'linux':
                    reap_detached_children()
            except (OSError, subprocess.TimeoutExpired) as error:
                record['error'] = 'cleanup failed: ' + str(error)
                result, status = result or 1, 'cleanup-failed'
        record.update(status=status, exit_code=result,
                      elapsed_seconds=round(time.monotonic() - started, 3))
        report.write_text(json.dumps(record, indent=2) + '\n')
        print('CI_PHASE_RESULT=' + json.dumps(record), flush=True)
    return result


if __name__ == '__main__':
    raise SystemExit(main())
