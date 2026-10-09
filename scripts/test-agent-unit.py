#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run agent unit tests without inheriting the invoking agent's process ancestry."""
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import threading
import time
import uuid


def interrupted(signum, _frame):
    raise SystemExit(128 + signum)


def run_tests(all_packages=False):
    with tempfile.TemporaryDirectory(prefix="fm-unit-", dir="/tmp") as directory:
        root = Path(directory)
        env = {k: v for k, v in os.environ.items() if not k.startswith("FLOWMUX_")}
        for key, child in [("FLOWMUX_RUNTIME_DIR", "run"), ("XDG_RUNTIME_DIR", "run"),
                           ("XDG_CONFIG_HOME", "config"),
                           ("XDG_DATA_HOME", "data"), ("XDG_STATE_HOME", "state"),
                           ("XDG_CACHE_HOME", "cache")]:
            (root / child).mkdir(mode=0o700, exist_ok=True)
            env[key] = str(root / child)
        command = ["cargo", "test", "--locked"]
        if not all_packages:
            command += ["--lib", "--bins", "-p", "flowmux-core", "-p", "flowmux-daemon",
                        "-p", "flowmux-cli", "-p", "flowmux-procmon"]
        return subprocess.run(
            command,
            cwd=Path(__file__).resolve().parents[1], env=env, timeout=900,
        ).returncode


def main(all_packages=False):
    if sys.platform == 'linux':
        # Linux subreapers can adopt a double-forked worker under the same agent.
        unit = 'flowmux-unit-' + uuid.uuid4().hex + '.service'
        command = ['systemd-run', '--user', '--wait', '--pipe', '--collect',
                   '--unit=' + unit,
                   '--service-type=exec', '--property=RuntimeMaxSec=960',
                   '--property=TimeoutStopSec=5', '--property=KillMode=control-group',
                   '--property=WorkingDirectory=' + str(Path(__file__).resolve().parents[1])]
        build_env = {'PATH', 'CARGO_HOME', 'CARGO_TARGET_DIR', 'CARGO_BUILD_JOBS',
                     'CARGO_INCREMENTAL', 'RUSTUP_HOME', 'RUSTUP_TOOLCHAIN',
                     'RUSTC', 'RUSTDOC', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
                     'RUSTFLAGS', 'RUSTDOCFLAGS', 'CARGO_ENCODED_RUSTFLAGS',
                     'CARGO_ENCODED_RUSTDOCFLAGS', 'CC', 'CXX', 'AR',
                     'PKG_CONFIG_PATH', 'PKG_CONFIG_LIBDIR', 'PKG_CONFIG_SYSROOT_DIR'}
        for key, value in os.environ.items():
            if key in build_env or key.startswith(('CARGO_PROFILE_', 'CARGO_TARGET_')):
                command.append('--setenv=' + key + '=' + value)
        command += [sys.executable, str(Path(__file__).resolve()), '--worker']
        if all_packages:
            command.append('--all')

        previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
        try:
            for sig in previous:
                signal.signal(sig, interrupted)
            return subprocess.run(command, timeout=970).returncode
        finally:
            # Killing systemd-run alone leaves its independently owned service alive.
            for sig in previous:
                signal.signal(sig, signal.SIG_IGN)
            try:
                result = subprocess.run(['systemctl', '--user', 'stop', unit],
                                        capture_output=True, text=True, timeout=15)
                # --collect may already have unloaded a completed service.
                if result.returncode not in (0, 5):
                    raise RuntimeError('test service cleanup failed: ' + result.stderr)
            finally:
                for sig, handler in previous.items():
                    signal.signal(sig, handler)
    # setsid alone leaves the Codex app-server among the ancestors. A short
    # intermediate process lets only our test worker be reparented to init.
    read_fd, write_fd = os.pipe()
    release_read, release_write = os.pipe()
    intermediate = os.fork()
    if intermediate == 0:
        os.close(read_fd)
        os.close(release_write)
        if os.fork():
            os._exit(0)
        os.setsid()

        def parent_gone():
            # Watch the existing pipe during Cargo too, not just after it exits.
            os.read(release_read, 1)
            os.killpg(os.getpgrp(), signal.SIGKILL)

        parent_watch = threading.Thread(target=parent_gone, daemon=True)
        parent_watch.start()
        os.write(write_fd, f"{os.getpid()}\n".encode())
        result = 1
        try:
            deadline = time.monotonic() + 5
            while os.getppid() != 1:
                if time.monotonic() > deadline:
                    raise RuntimeError("test worker was not reparented")
                time.sleep(.01)
            result = run_tests(all_packages)
        except Exception as error:
            print(f"agent unit runner failed: {error}", flush=True)
        finally:
            try:
                os.write(write_fd, f"{result}\n".encode())
            except BrokenPipeError:
                pass
            os.close(write_fd)
            # Keep the group leader alive until the parent cleans the group:
            # macOS killpg returns EPERM for a group containing only a zombie.
            parent_watch.join()
            os._exit(result)
    os.close(write_fd)
    os.close(release_read)
    os.waitpid(intermediate, 0)
    with os.fdopen(read_fd) as result_stream:
        worker = int(result_stream.readline())
        previous = {sig: signal.getsignal(sig) for sig in (signal.SIGINT, signal.SIGTERM)}
        try:
            for sig in previous:
                signal.signal(sig, interrupted)
            status = result_stream.readline()
            return int(status) if status else 1
        finally:
            # A result only proves Cargo exited, not its descendants. The worker
            # owns a private group; also remove children that ignore SIGTERM.
            try:
                os.killpg(worker, signal.SIGKILL)
            except ProcessLookupError:
                pass
            finally:
                os.close(release_write)
                for sig, handler in previous.items():
                    signal.signal(sig, handler)


if __name__ == "__main__":
    if sys.argv[1:] in (["--worker"], ["--worker", "--all"]):
        for sig in (signal.SIGINT, signal.SIGTERM):
            signal.signal(sig, interrupted)
        raise SystemExit(run_tests(all_packages='--all' in sys.argv[1:]))
    if sys.argv[1:] not in ([], ["--all"]):
        raise SystemExit('Usage: test-agent-unit.py [--all]')
    raise SystemExit(main(all_packages=sys.argv[1:] == ["--all"]))
