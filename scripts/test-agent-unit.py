#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run agent unit tests without inheriting the invoking agent's process ancestry."""
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time


def run_tests():
    with tempfile.TemporaryDirectory(prefix="fm-unit-", dir="/tmp") as directory:
        root = Path(directory)
        env = {k: v for k, v in os.environ.items() if not k.startswith("FLOWMUX_")}
        for key, child in [("FLOWMUX_RUNTIME_DIR", "run"), ("XDG_CONFIG_HOME", "config"),
                           ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"),
                           ("XDG_CACHE_HOME", "cache")]:
            (root / child).mkdir(mode=0o700)
            env[key] = str(root / child)
        return subprocess.run(
            ["cargo", "test", "--locked", "--lib", "--bins", "-p", "flowmux-core",
             "-p", "flowmux-daemon", "-p", "flowmux-cli", "-p", "flowmux-procmon"],
            cwd=Path(__file__).resolve().parents[1], env=env, timeout=900,
        ).returncode


def main():
    # setsid alone leaves the Codex app-server among the ancestors. A short
    # intermediate process lets only our test worker be reparented to init.
    read_fd, write_fd = os.pipe()
    intermediate = os.fork()
    if intermediate == 0:
        os.close(read_fd)
        if os.fork():
            os._exit(0)
        os.setsid()
        os.write(write_fd, f"{os.getpid()}\n".encode())
        result = 1
        try:
            deadline = time.monotonic() + 5
            while os.getppid() != 1:
                if time.monotonic() > deadline:
                    raise RuntimeError("test worker was not reparented")
                time.sleep(.01)
            result = run_tests()
        except Exception as error:
            print(f"agent unit runner failed: {error}", flush=True)
        finally:
            os.write(write_fd, f"{result}\n".encode())
            os.close(write_fd)
            os._exit(result)
    os.close(write_fd)
    os.waitpid(intermediate, 0)
    with os.fdopen(read_fd) as result_stream:
        worker = int(result_stream.readline())
        finished = False
        try:
            status = result_stream.readline()
            finished = bool(status)
            return int(status) if status else 1
        finally:
            # Interrupt only our still-running private test process group.
            if not finished:
                try:
                    os.killpg(worker, signal.SIGTERM)
                except ProcessLookupError:
                    pass


if __name__ == "__main__":
    raise SystemExit(main())
