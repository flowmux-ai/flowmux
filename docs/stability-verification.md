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

`cargo deny --locked --config packaging/licenses/deny.toml check advisories
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
settings. Four runner tests cover nonzero results, timeout cleanup,
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

## 5. Advisory CI gate

The existing license workflow now checks advisories, licenses and sources in
one command. Corrected cargo-deny's `--config` placement in CI and contributor
instructions. Dependency push/PR paths, weekly schedule and release workflow
reuse remain intact; asset and editor license checks are unchanged.

Verification: the exact CI command passes on this lockfile. A separate temporary
crate pinned to anyhow 1.0.102 fails with exit 1 and RUSTSEC-2026-0190, proving
that an affected dependency blocks the gate. YAML parsing and assertions checked
the command and all five trigger types. No advisory suppression was added.
Regression review: no runtime code changes; future advisories intentionally fail
both dependency updates and scheduled/release audits. GitHub-hosted execution
itself was not triggered from this local branch.

## 6. Remaining risk validation

### Inspector address validation — confirmed and fixed

With automation set to `0`, a real browser still started WebKit's loopback
inspector listener. The old validator returned before inspecting the address.
It now validates every supplied address before WebKit initialization, regardless
of opt-in state. Unit coverage includes unset/0/1, IPv4/IPv6 loopback, wildcard,
non-loopback, malformed, empty and zero-port addresses. The live GUI retains
loopback inspection with automation disabled and refuses wildcard addresses
in all three modes before opening a listener. No public inspector was exposed
to reproduce the old behavior. Normal opt-in native WebDriver still passes.

### Same-boot PID reuse — confirmed and fixed

The isolated live fixture substituted an owned, unrelated `sleep` process for a
closed window's PID. Before the change, its saved workspace disappeared from
the restored UI. Window records now include an optional process start marker
(Linux procfs ticks; macOS process birth microseconds). A differing marker
permits recovery without signaling the unrelated process. Existing live window
ownership, boot handling, atomic merging and workspace contents are preserved.
The live test now checks both recovery and a second window not stealing the
first window's workspace; it runs in the existing session-save CI suite.

Regression review: parser tests include parentheses/spaces in process names,
truncation and invalid numbers. State tests cover matching/mismatching markers,
legacy JSON without the field, new claim/save metadata and existing migrations.
The optional field requires no schema bump. Missing or unreadable markers keep
the conservative PID-only behavior: already-reused PIDs in old files cannot be
disambiguated until new metadata is saved. No executable-name heuristic is used.
macOS API field types were checked against libc; its runtime was not available.

### WebKit lifecycle — no additional defect established

Native source confirms that automation uses an ephemeral network session and
clears its automation-session reference after `will-close`. Related views
inherit automation control; re-setting it is not a required fix. Existing popup
callbacks retain their widgets in the session map without adding an extra
return reference. No ownership or FFI rewrite was justified by these hypotheses.

Repeated the native smoke suite five times in one GUI (15 sessions), then
repeated with a monitor that identifies sandbox children by the private
inspector endpoint and verifies it detects them while sessions are active.
After every batch: one mapped main window, zero matching WebKit web/network
processes, no sandbox fallback or fatal GTK critical. GUI RSS in the second
run was 188,624 / 190,820 / 190,892 / 190,684 / 190,780 KiB. This bounded run
shows no accumulating windows/processes or continuing RSS growth; it is not
a heap-leak proof or an extended soak test. Native input and storage isolation
passed in all 30 sessions across the two runs.

### Poisoned save mutex — no production trigger established

Reviewed all synchronous save callers: GUI close uses `gio::spawn_blocking`;
asynchronous/autosave paths use `tokio::spawn_blocking`. None call the Tokio
`blocking_lock` from a runtime worker. State serialization and filesystem errors
propagate as `Result`; close failures keep the window open. Artificially
panicking while holding the mutex would demonstrate poisoning, not establish a
reachable user defect. Automatic poison recovery was therefore not added.
An unexpected future worker panic can still leave saves blocked by poison;
that limitation remains explicit rather than concealing a failed save.

## Final regression gate

After the inspector and process-identity changes, the complete Linux gate
passed again with `CI_GATE_EXIT=0`: 1,840 Rust tests passed, zero failed,
eight existing ignored; 42 live checks passed. Regions 83.18%, functions
82.68%, lines 83.69% satisfy the configured thresholds. LLVM still warns about
216 functions with mismatched data; retain the coverage precision limitation
noted above. Workspace build, locked all-target Clippy with warnings denied,
formatting and the combined advisory/license/source gate passed.

The separate worktree is `../flowmux-stability` on `fix/functional-stability`.
The original user GUI remained running throughout. The temporary Flatpak test
application was uninstalled; the shared runtime was retained. No merge, push
or user installation was performed. Local detailed evidence is retained in
`/tmp/flowmux-stability-ci-final.log`, `-pid-before.log`, `-pid-after.log`,
`-inspector-before.log`, `-inspector-after.log` and `-webdriver-lifecycle.log`
(the latter names share the `/tmp/flowmux-stability` prefix).

## Main integration verification

Replayed the six stability commits on main through `5d435938`. The first
combined GUI run exposed global application state left by the welcome shortcut
test: two later pane-tooltip tests incorrectly inherited its accelerators.
The welcome test now restores the previous default application on both normal
return and panic, and asserts restoration. Existing tooltip assertions remain
unchanged; no runtime behavior was modified by this integration fix.

The complete rerun passed (`CI_GATE_EXIT=0`): 1,845 Rust tests passed, zero
failed, eight existing ignored, and 42 live checks passed. Coverage was
83.08% regions, 82.60% functions and 83.60% lines. The earlier
LLVM function-data warning still applies. The log is retained at
`/tmp/flowmux-stability-main-ci-current.log`.

## IPC recovery regression follow-up

Main already contains the accept-retry implementation in `8aef7efe`. Applied
the remaining regression-test improvements from `9782151a` on top of `57a40caa`.
Each socket must log its own accept failure before restoring resource limits;
console colors are disabled so this check works in systemd services too.
The readiness marker requires actual shell execution, and the retry-count
limit accounts for elapsed time instead of assuming a fixed test duration.
Runtime code and Linux coverage-gate wiring are unchanged.

Verification passed with `VERIFY_EXIT=0`: workspace formatting, all-target
Clippy with warnings denied, the complete locked workspace test suite (zero
failures, eight existing ignored), and 11 live GUI checks covering both IPC
listeners, PTY allocation failure, session save/restore and PID reuse.
The run used a private systemd service, display and state with the installed
AppArmor profile. No regression was found; the active user GUI stayed alive.
The log is retained in `.worktrees/ipc-main-regression/target/review/verify.log`.
The full coverage/SSH gate and macOS checks were not rerun for this follow-up.
