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
