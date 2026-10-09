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

## 4. Sandboxed GUI and native WebDriver verification

The host has the `flowmux-webkit` AppArmor profile. `aa-exec -p flowmux-webkit`
allows WebKit's user namespaces without disabling the sandbox or changing host
policy. A full gate launched directly below Codex hung in three hook tests that
inspect real agent ancestry; the isolated headless runner passed them. Run the
full gate in a dedicated systemd user service too, with a runtime ceiling and
the separate build target. The normal hosted CI environment has no agent parent.

Native input was verified against GNOME Platform 49's WebKitGTK/WebKitWebDriver
2.54.1. A minimal, private `com.flowmux.Stability` test application references
that runtime. A private D-Bus session inherits the test Flatpak installation's
data directory, allowing its spawn portal to find the application. Running a
runtime directly is insufficient: its metadata has no `Application` group and
WebKit warns that subprocess sandboxing is unavailable. The installed test
application passes `flatpak-spawn --sandbox true` and the repository WebDriver
smoke test with fatal GTK criticals enabled, without sandbox fallback warnings.

The native test verifies trusted mouse/key/input events, Korean editing,
Enter/Tab, separate windows, popup closure, screenshots, three sessions,
cookie/localStorage isolation and disposing a still-open popup at session end.
The GUI remains alive afterward. Ordinary user windows and state are untouched.
The Ubuntu host's 2.52.6 library still lacks native WebDriver input; this is a
runtime build limitation, not a reason to substitute JavaScript input.

A second full run encountered transient host disk exhaustion: four GUI tests
failed while creating temporary files. That run is failed evidence, not a
passing gate; its log is retained separately. Coverage also reports mismatched
function data between compiled variants, so the aggregate percentages are gate
measurements, not a claim of exact per-function coverage.

The complete rerun passed with `CI_GATE_EXIT=0`: 1,837 Rust tests passed,
eight existing tests ignored, and 41 live checks passed (session persistence,
PTY failure, both IPC endpoints, editor I/O, SSH/tmux lifecycle and native hook
replay). Coverage: regions 83.17%, functions 82.69%, lines 83.68%, above all
three configured floors. The SSH fixture used private sshd/tmux executables
on its service PATH, not changes to user SSH configuration.
Regression review: the full suite and native driver use isolated state and
owned process handles; no user window was closed. macOS was not executed.
