#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Bound one Linux/WSL build or check; use run-check.ps1 for Windows children."""
import argparse
import json
import os
from pathlib import Path
import signal
import shutil
import subprocess
import sys
import time
import uuid


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--name', default='check')
    parser.add_argument('--timeout-seconds', type=int, default=120)
    parser.add_argument('--keep-artifacts', action='store_true', help='retain successful check logs and artifacts')
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command or not 1 <= args.timeout_seconds <= 1800:
        parser.error('a command and a deadline of 1..1800 seconds are required')
    if not all(c.isascii() and (c.isalnum() or c in '_-') for c in args.name):
        parser.error('name must contain only ASCII letters, digits, underscores or hyphens')
    if os.name != 'posix' or command[0].lower().endswith(('.exe', '.com')):
        parser.error('use run-check.ps1 for Windows processes and their descendants')
    directory = Path(__file__).resolve().parent.parent / 'dist' / 'checks' / (args.name + '-' + str(uuid.uuid4()))
    directory.mkdir(parents=True)
    artifact_root = directory / 'artifacts'
    artifact_root.mkdir()
    environment = os.environ.copy()
    environment['FLOWMUX_TEST_ARTIFACT_ROOT'] = str(artifact_root)
    result = dict(name=args.name, command=command, deadlineSeconds=args.timeout_seconds, status='runner_error')
    started = time.monotonic()
    process = None
    cancelled_by = None
    def request_cancel(signum, _frame):
        nonlocal cancelled_by
        # Do not raise from Popen before it has returned the owned PID, or
        # interrupt cleanup when cancellation is requested more than once.
        if cancelled_by is None:
            cancelled_by = signum
    previous_handlers = {
        signum: signal.signal(signum, request_cancel)
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP)
    }
    def terminate_group():
        if process is not None:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
    try:
        with (directory / 'stdout.txt').open('wb') as output, (directory / 'stdout.txt').open('rb') as reader:
            process = subprocess.Popen(command, stdout=output, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL, start_new_session=True, env=environment)
            result['pid'] = process.pid
            print(f'[{args.name}] started pid={process.pid}, deadline={args.timeout_seconds}s; logs: {directory}', flush=True)
            heartbeat = 5
            while True:
                if cancelled_by is not None:
                    break
                chunk = reader.read(65536)
                if chunk:
                    sys.stdout.buffer.write(chunk)
                    sys.stdout.buffer.flush()
                elapsed = time.monotonic() - started
                code = process.poll()
                if code is not None:
                    result.update(status='passed' if code == 0 else 'failed', exitCode=code)
                    break
                if elapsed >= args.timeout_seconds:
                    result.update(status='timeout', exitCode=124)
                    break
                if elapsed >= heartbeat:
                    print(f'[{args.name}] running {elapsed:.1f}/{args.timeout_seconds}s', flush=True)
                    heartbeat = elapsed + 5
                time.sleep(0.1)
            terminate_group()
            process.wait(timeout=3)
            while cancelled_by is None and (chunk := reader.read(65536)):
                sys.stdout.buffer.write(chunk)
            sys.stdout.buffer.flush()
    except (Exception, KeyboardInterrupt) as error:
        result.update(status='runner_error', exitCode=125, error=str(error))
    finally:
        terminate_group()
        if cancelled_by is not None and 'error' not in result:
            result.update(status='cancelled', exitCode=128 + cancelled_by,
                          signal=signal.Signals(cancelled_by).name)
        result['elapsedSeconds'] = round(time.monotonic() - started, 3)
        retained = args.keep_artifacts or result['status'] != 'passed'
        try:
            if not retained:
                try:
                    shutil.rmtree(directory)
                except OSError as error:
                    retained = True
                    result.update(status='cleanup_failed', exitCode=125, cleanupError=str(error))
            if retained:
                directory.mkdir(parents=True, exist_ok=True)
                (directory / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
            location = f'result: {directory / "result.json"}' if retained else 'temporary logs and artifacts removed'
            print(f'[{args.name}] {result["status"]} in {result["elapsedSeconds"]}s; {location}', flush=True)
        finally:
            for signum, handler in previous_handlers.items():
                signal.signal(signum, handler)
    return result['exitCode'] if 0 <= result['exitCode'] <= 255 else 1


if __name__ == '__main__':
    sys.exit(main())
