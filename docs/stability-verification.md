<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Functional stability verification

Work is isolated on `fix/functional-stability`, based on `59f0502b`.
The user's running GUI is never restarted. Linux GUI checks use the installed
`flowmux-webkit` AppArmor profile with private display, state and runtime paths.

## 1. IPC accept recovery

Reproduced on the baseline GUI by exhausting its file descriptors: the server
logged `ipc server exited: Too many open files` and refused further connections.
Accept failures now wait 100 ms and retry; initial socket bind errors still
propagate. Existing handlers, connection limits and mutation admission are unchanged.

Verification: 76 IPC tests, IPC clippy, workspace formatting, live recovery on
both socket endpoints, and the existing PTY spawn failure GUI regression passed.
The new GUI check is included in the Linux coverage gate. It verifies admitted
requests, queued requests, bounded retry frequency, unchanged workspace state,
terminal input and new pane creation after resource recovery.

Regression review: an accept error delays both listeners by at most one retry
interval, but does not delay admitted handlers. Persistent exhaustion remains
unserviceable until resources are freed; retries are bounded to ten per second.
The test restores only its own GUI's original resource limits in a `finally`
block. It does not raise process limits or cancel in-flight mutations.

## 2. Dependency advisories

Updated anyhow to 1.0.103, rustls to 0.23.45, quick-xml to 0.41.0 and gix to
0.77.0 (the first gix release using fixed gix-date 0.12). The locked graph uses
gix-date 0.12.1 and gix-features 0.45.2. Removed unused Comrak default features;
the renderer uses `markdown_to_html`, not its optional Syntect adapter or CLI.
The explicitly retained `shortcodes` feature preserves emoji rendering. This
also removes unmaintained bincode/yaml-rust and the vulnerable indirect XML
parser, without advisory exceptions.

Regression review: gix repository/worktree discovery is covered by 15 tests;
Markdown rendering by four tests, including escaped fenced code and emoji.
Eleven scrollback tests include actual VTE export/replay, styled minimap pixels,
malformed/oversized input and HTML attribute whitespace. Attribute decoding
retains the previous HTML behavior instead of applying XML normalization.
TLS configuration is unchanged; the TLS implementation receives its patch
update. This does not constitute a separate cryptographic audit.

`cargo deny --locked check --config packaging/licenses/deny.toml advisories
licenses sources` passes with zero exceptions. The distribution notice was
regenerated with the required cargo-about 0.9.2; four notice tests pass.
Workspace build, all-target Clippy, formatting and live degraded/healthy session
save-and-restore checks also pass on the updated dependency graph.

## 3. Linux test runner lifecycle

Reproduced the original behavior: terminating the `systemd-run` client left its
service running, and caller build settings did not reach the worker. Each run
now names its own service with a UUID and stops that service on completion,
interrupt or timeout. Systemd kills the entire service cgroup after a five-second
grace period. Worker signal handling unwinds its temporary state directory.
An explicit allowlist forwards build/toolchain settings without copying agent
context or caller registry tokens. No fallback weakens the systemd isolation.

Regression review: real SIGINT/SIGTERM tests verify Cargo and a child that
ignores SIGTERM disappear, the service becomes inactive, and temporary state
is removed. They also check a build directory containing spaces and profile
settings. Thirteen pre-push/runner tests cover nonzero results, timeout cleanup,
environment filtering and cleanup failures. Cleanup failure cannot report
success. Signal handlers are restored, and macOS retains its existing process
ancestry strategy.
The updated runner's real `--all` headless run also passes (1,084 tests;
eight existing ignored tests), with the caller's separate build cache.

SIGKILL cannot run Python cleanup; the independent service still has its
960-second runtime ceiling. A failed user service manager remains an error.
