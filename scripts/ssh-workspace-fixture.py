#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Isolated, unprivileged OpenSSH integration checks; --keep serves GUI tests.

Example with distribution packages extracted into /tmp/ssh-tools:
  python3 scripts/ssh-workspace-fixture.py \
    --sshd /tmp/ssh-tools/usr/sbin/sshd \
    --tmux /tmp/ssh-tools/usr/bin/tmux \
    --library-path /tmp/ssh-tools/usr/lib/x86_64-linux-gnu --keep

No user SSH configuration, installed binaries, or flowmux processes are changed.
Stop a kept fixture with SIGTERM; it cleans up only its own resources.
"""

import argparse
import http.server
import json
import os
from pathlib import Path
import pwd
import shlex
import shutil
import signal
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request


def run(argv, **kwargs):
    result = subprocess.run(argv, text=True, capture_output=True, timeout=15, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{shlex.join(argv)}: {result.stderr.strip()}")
    return result.stdout.strip()


def wait_for(predicate, description):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.05)
    raise AssertionError(f"Timed out: {description}")


def unused_port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sshd", default=shutil.which("sshd"))
    parser.add_argument("--tmux", default=shutil.which("tmux"))
    parser.add_argument("--library-path")
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    if not args.sshd or not args.tmux:
        parser.error("Provide --sshd and --tmux, or install both in PATH")
    env = os.environ.copy()
    if args.library_path:
        env["LD_LIBRARY_PATH"] = args.library_path
    children = []
    httpd = None
    tmux = None

    def stop(_signum, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, stop)
    with tempfile.TemporaryDirectory(prefix="fm-ssh-") as temporary:
        root = Path(temporary)
        remote = root / "remote workdir"
        remote.mkdir()
        remote_bin = root / "bin"
        remote_bin.mkdir()
        tmux_program = (["env", f"LD_LIBRARY_PATH={args.library_path}"] if args.library_path else []) + [str(Path(args.tmux).resolve())]
        tmux_wrapper = remote_bin / "tmux"
        tmux_wrapper.write_text("#!/bin/sh\nexec " + shlex.join(tmux_program + ["-S", str(root / "gui-tmux.sock")]) + ' "$@"\n')
        tmux_wrapper.chmod(0o700)
        port = unused_port()
        user = pwd.getpwuid(os.getuid()).pw_name
        for name in ("host", "client"):
            run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(root / name)])
        public = (root / "host.pub").read_text().split()
        (root / "known_hosts").write_text(f"[127.0.0.1]:{port} {public[0]} {public[1]}\n")
        config = root / "sshd_config"
        config.write_text(f"""Port {port}
ListenAddress 127.0.0.1
HostKey {root}/host
PidFile {root}/sshd.pid
AuthorizedKeysFile {root}/client.pub
# The generated key is inside a mode-0700 temporary directory under /tmp.
StrictModes no
PasswordAuthentication no
KbdInteractiveAuthentication no
PubkeyAuthentication yes
UsePAM no
AllowUsers {user}
AllowTcpForwarding local
GatewayPorts no
X11Forwarding no
PermitUserEnvironment no
SetEnv PATH={remote_bin}:/usr/bin:/bin FLOWMUX_SSH_FIXTURE={root} LANG=C.UTF-8
LogLevel VERBOSE
""")
        client_config = root / "ssh_config"
        client_config.write_text(f"""Host flowmux-fixture
    HostName 127.0.0.1
    Port {port}
    User {user}
    IdentityFile {root}/client
    IdentitiesOnly yes
    UserKnownHostsFile {root}/known_hosts
    GlobalKnownHostsFile /dev/null
    StrictHostKeyChecking yes
    ConnectTimeout 5
""")
        client_config.chmod(0o600)
        control = str(root / "control")
        host = f"{user}@127.0.0.1"
        base = ["ssh", "-F", "/dev/null", "-p", str(port), "-i", str(root / "client"),
                "-o", "IdentitiesOnly=yes", "-o", f"UserKnownHostsFile={root}/known_hosts",
                "-o", "GlobalKnownHostsFile=/dev/null", "-o", "StrictHostKeyChecking=yes",
                "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-S", control]
        slave = base + ["-o", "ControlMaster=no", "-o", "ProxyCommand=/bin/false"]
        tmux = [str(Path(args.tmux).resolve()), "-S", str(root / "tmux.sock")]
        remote_tmux = (["env", f"LD_LIBRARY_PATH={args.library_path}"] if args.library_path else []) + tmux
        tmux_command = shlex.join(remote_tmux)
        try:
            with (root / "sshd.log").open("w") as log:
                sshd = subprocess.Popen([str(Path(args.sshd).resolve()), "-D", "-e", "-f", str(config)],
                                        env=env, stdout=log, stderr=log, start_new_session=True)
                children.append(sshd)
            time.sleep(0.2)
            if sshd.poll() is not None:
                raise RuntimeError((root / "sshd.log").read_text())
            master = subprocess.Popen(base + ["-M", "-N", "-T", "-o", "ControlPersist=no", host],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            children.append(master)
            wait_for(lambda: Path(control).exists() or master.poll() is not None, "master socket")
            if master.poll() is not None:
                raise RuntimeError(master.stderr.read().decode() + (root / "sshd.log").read_text())
            assert Path(control).stat().st_mode & 0o077 == 0, "Control socket is not private"
            run(base + ["-O", "check", host])

            command = f"cd {shlex.quote(str(remote))} && printf '%s\\n' \"$SSH_CONNECTION\" && pwd"
            channels = [subprocess.Popen(slave + [host, command + "; read answer; printf '%s\\n' \"$answer\""],
                                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                         text=True) for _ in range(2)]
            children.extend(channels)
            for channel in channels:
                assert channel.stdout.readline().strip().endswith(f"127.0.0.1 {port}")
                assert channel.stdout.readline().strip() == str(remote)
            channels[0].communicate("first\n", timeout=5)
            output, _ = channels[1].communicate("second-survives\n", timeout=5)
            assert output.strip() == "second-survives"
            run(base + ["-O", "check", host])
            print("PASS: two SSH channels share a private master; closing one preserves its sibling", flush=True)

            class Handler(http.server.BaseHTTPRequestHandler):
                def do_GET(self):
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(b"flowmux-remote-preview")

                def log_message(self, *_args):
                    pass

            httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
            threading.Thread(target=httpd.serve_forever, daemon=True).start()
            local_port = unused_port()
            forwarding = f"127.0.0.1:{local_port}:127.0.0.1:{httpd.server_port}"
            run(base + ["-O", "forward", "-L", forwarding, host])
            with urllib.request.urlopen(f"http://127.0.0.1:{local_port}", timeout=5) as response:
                assert response.read() == b"flowmux-remote-preview"
            run(base + ["-O", "cancel", "-L", forwarding, host])
            with socket.socket() as probe:
                assert probe.connect_ex(("127.0.0.1", local_port)) != 0, "Forward listener survived cancel"
            print("PASS: live master adds and cancels a loopback HTTP forward", flush=True)

            run(slave + [host, f"{tmux_command} new-session -d -s fixture 'exec sleep 600'"])
            pane_pid = run(slave + [host, f"{tmux_command} display-message -p -t fixture '#{{pane_pid}}'"])
            for attempt in range(2):
                if attempt:
                    run(base + ["-O", "exit", host])
                    master.wait(timeout=5)
                    master = subprocess.Popen(base + ["-M", "-N", "-T", "-o", "ControlPersist=no", host],
                                              stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
                    children.append(master)
                    wait_for(lambda: Path(control).exists(), "replacement master socket")
                attachment = subprocess.Popen(slave + ["-tt", host, f"TERM=xterm {tmux_command} attach-session -t fixture"],
                                              stdin=subprocess.PIPE, stdout=subprocess.DEVNULL,
                                              stderr=subprocess.DEVNULL)
                children.append(attachment)
                wait_for(lambda: bool(run(slave + [host, f"{tmux_command} list-clients -t fixture"])), "tmux attach")
                run(slave + [host, f"{tmux_command} detach-client -s fixture"])
                attachment.wait(timeout=5)
                assert run(slave + [host, f"{tmux_command} display-message -p -t fixture '#{{pane_pid}}'"]) == pane_pid
            run(slave + [host, f"{tmux_command} kill-session -t fixture"])
            missing = subprocess.run(slave + [host, f"{tmux_command} attach-session -t fixture"],
                                     capture_output=True, timeout=5)
            assert missing.returncode != 0, "Missing tmux session was recreated"
            print("PASS: tmux preserves its process across master reconnect; missing-session attach fails", flush=True)

            # A missing master must fail closed instead of authenticating a new connection.
            run(base + ["-O", "exit", host])
            master.wait(timeout=5)
            fallback = subprocess.run(slave + [host, "printf unexpected-fallback"], capture_output=True, timeout=5)
            assert fallback.returncode != 0 and b"unexpected-fallback" not in fallback.stdout
            print("PASS: a slave fails closed after master exit", flush=True)
            if args.keep:
                print(json.dumps({"root": str(root), "fixture_pid": os.getpid(), "sshd_pid": sshd.pid,
                                  "host": host, "port": port, "identity_file": str(root / "client"),
                                  "known_hosts": str(root / "known_hosts"), "ssh_config": str(client_config),
                                  "remote_cwd": str(remote), "http_port": httpd.server_port,
                                  "remote_bin": str(remote_bin), "tmux": tmux,
                                  "library_path": args.library_path}), flush=True)
                print("Fixture ready; SIGTERM this fixture process to clean up.", flush=True)
                while True:
                    signal.pause()
        finally:
            if tmux:
                subprocess.run(tmux + ["kill-server"], env=env, stdout=subprocess.DEVNULL,
                               stderr=subprocess.DEVNULL, timeout=5)
                subprocess.run(tmux_program + ["-S", str(root / "gui-tmux.sock"), "kill-server"],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5)
            for child in reversed(children):
                if child.poll() is None:
                    if child is sshd:
                        os.killpg(child.pid, signal.SIGTERM)
                    else:
                        child.terminate()
                    try:
                        child.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.wait(timeout=5)
            if httpd:
                httpd.shutdown()


if __name__ == "__main__":
    try:
        main()
    except KeyboardInterrupt:
        pass
