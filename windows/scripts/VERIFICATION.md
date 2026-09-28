<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Background verification

Run the smallest test affected by a change, then related regressions once.
Reuse passing results while the source is unchanged. Build terminal/editor
assets before Rust compilation; keep release builds out of the debug loop.
One lead schedules one compiler and one Windows suite at a time. Give parallel
agents concrete file scopes and a 60-second checkpoint; interrupt stalled work.

Tests use hidden windows, isolated state and owned IPC. Do not inject desktop
input, use the desktop clipboard, or replace a running application.
Runtime/UI changes require a live hidden Windows check; unit tests alone do not
establish visible UI or physical Korean IME behavior.

```powershell
.\windows\scripts\run-check.ps1 -Name theme -TimeoutSeconds 60 `
  -Path .\windows\scripts\verify-theme.ps1
```

```sh
python3 windows/scripts/run-check.py --name tests --timeout-seconds 120 -- \
  cargo test --manifest-path windows/Cargo.toml --locked
```

Successful checks print their result and delete their temporary output.
Failures retain logs at the printed path for diagnosis. Use
`-KeepArtifacts` (PowerShell) or `--keep-artifacts` (Python) only when those
outputs are needed. Native verifiers inherit the runner's temporary artifact
root. Do not commit logs, screenshots, source-hash snapshots or per-step reports,
or update documentation after each edit. Keep only useful build/usage guidance.

The Windows runner starts its target hidden in a kill-on-close Job Object.
The POSIX runner uses a process group; it cannot contain Windows interop or
intentionally detached processes. Both clean up owned descendants, report
progress, and enforce deadlines. Timeout is failure, never an automatic retry.
Inspect the cause before retrying or extending a deadline.

Native Rust tests require bundled ConPTY beside the test executable:
`python3 windows/scripts/fetch-conpty.py --output <test-executable-directory>`.
Run the existing `verify-check-runner.ps1` and `verify-check-runner-posix.py`
when changing runner lifecycle behavior.
