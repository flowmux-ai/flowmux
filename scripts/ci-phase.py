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
        process = subprocess.Popen(command, start_new_session=True)
        record['pid'] = process.pid
        report.write_text(json.dumps(record, indent=2) + '\n')
        result = process.wait(timeout=timeout)
        if result < 0:
            result = 128 - result
        status = 'passed' if result == 0 else 'failed'
    except subprocess.TimeoutExpired:
        result, status = 124, 'timeout'
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
