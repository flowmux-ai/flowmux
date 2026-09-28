<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Bounded background verification

Use these runners for ongoing Windows work. Do not call an ad-hoc build loop
with unbounded `subprocess.run`, `Start-Process -Wait`, or a bare verifier.
Tests remain hidden, use isolated state and owned IPC, and never inject desktop
input or touch the user clipboard. A timeout is a failure with diagnostics;
it is never a passing result and is never automatically retried.

1. Run the smallest affected pure test/check first. For Rust changes, build the
   debug Windows binary once and immediately run the affected native verifier.
   For script-only changes, reuse the existing binary. Record which binary was
   used; do not imply it includes newer Rust changes.
   Run Windows-target Clippy before expensive native test linking/debug builds
   to catch target-specific imports and lint failures early. Reuse that result
   at the commit boundary when its source inputs stay unchanged. Run cheap native
   handle/path tests as soon as the test binary exists, before broader UI suites.
2. Stop on the first failure. Inspect the failed step's stdout/stderr, result
   JSON and native evidence before changing anything. Retry only that failed
   step after a relevant fix. Keep the earlier evidence. Do not lengthen a
   timeout without a specific cause and measured justification.
3. After the affected test passes, run related regressions once. The quick
   browser-wait suite explicitly records the deferred 27-second transport
   test. Use `-Extended` for transport/deadline changes and final release
   validation; this deliberate long-duration assertion must not be shortened.
4. At a commit/release boundary, run required Clippy, native tests, release
   build and packaging once for the final source. Reuse successful results
   while their inputs and artifacts are unchanged. Do not rebuild a debug
   executable while a live test is using it. Never run desktop suites in
   parallel: they share machine resources even when their hosts are isolated.

Windows PowerShell 5.1 examples:

```powershell
.\windows\scripts\run-check.ps1 -Name downloads -TimeoutSeconds 120 `
  -Path .\windows\scripts\verify-browser-downloads.ps1
.\windows\scripts\run-check.ps1 -Name browser-wait -TimeoutSeconds 120 `
  -Path .\windows\scripts\verify-browser-wait.ps1 -ArgumentList '-Extended'
.\windows\scripts\run-check.ps1 -Name native-tests -TimeoutSeconds 60 `
  -Path <native-test.exe> -ArgumentList '--test-threads=1'
```

Linux/WSL build example (the Python runner is for POSIX process trees only):

```sh
python3 windows/scripts/run-check.py --name linux-tests --timeout-seconds 120 -- \
  cargo test --manifest-path windows/Cargo.toml --locked
```

Default limits: ordinary native suite 120s, native unit tests 60s, incremental
build/Clippy 300s, packaging 60s. A cold dependency build may need an explicit
larger deadline (maximum 1800s); it still reports progress. Both runners print
each step's output immediately, heartbeat every five seconds, and write a
unique `windows/dist/checks/<name>-<uuid>/result.json`. Windows records cleanup
counts and the original exit code separately from timeout (124) and runner or
cleanup failure (125). There are no retries or silent skips.

The Windows runner starts the target suspended and hidden, assigns an unnamed
kill-on-close Job Object, then resumes it. Timeout, error and normal completion
all clean up remaining owned descendants; cleanup observation is capped at
three seconds. No process-name matching or blanket termination is used. This
also guards older verifier internals when invoked through the runner. It does
not authorize running an interactive/foreground verifier in the background.
The runner is a test harness, not an installed application component.

The active browser/key suites additionally use bounded post-kill waits and
nonblocking diagnostic output reads. Download polling uses a five-second IPC
budget for ordinary commands. Completion checks return as soon as their
condition is met; only tests of elapsed-time behavior have a deliberate dwell.

## Parallel agent handoffs

Give each agent an explicit file scope and a concrete handoff. Request a progress
checkpoint within 60 seconds. During an implementation or verification handoff,
check outstanding agents at least once per minute while doing independent work.
If an agent has no concrete progress or identified blocker for two minutes,
interrupt it, inspect its existing changes, and take over or reassign the bounded
remainder. A reported external blocker does not justify waiting without a limit.
Optional reviews and next-stage planning must never hold up an otherwise ready
build; cap those handoffs at two minutes and carry remaining notes separately.

Freeze the affected source before compiling. One lead owns compiler and native
suite scheduling; other agents can prepare documentation, evidence and later
plans without changing those inputs. Stop only the stalled owned agent or test,
preserve partial work and failure logs, and resume at the smallest pending check.

Self-check: `verify-check-runner.ps1` verifies success, preserved exit status,
and timeout of a process with a live descendant. Inspect `activeAfterCleanup`
for the timeout result. The POSIX runner uses a separate process group and
kills that group on normal/error/timeout exits and SIGTERM/SIGHUP/SIGINT
cancellation. `verify-check-runner-posix.py --output <evidence.json>` exercises
cancellation with children and grandchildren. SIGKILL cannot be handled; the
runner also cannot contain intentionally detached processes or Windows processes
launched through WSL interop.

Job containment follows Microsoft's [Job Objects documentation](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)
and [process creation flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags).
