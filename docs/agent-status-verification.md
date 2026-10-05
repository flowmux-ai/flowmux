<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Agent status verification

Keep the user's running sessions intact. Use a rebuilt isolated GUI, separate
XDG state/config/data and a short `FLOWMUX_RUNTIME_DIR` under `/tmp` on macOS.

## Decision evidence

Enable `FLOWMUX_LOG=warn,flowmux_agent=debug` on the isolated GUI and hook CLI.
This uses existing console/daily logs and their bounded writer queue; it adds no
persistent event database. Debug capture is opt-in and is not a history of events
that happened before it was enabled. Retain logs with the tested Git revision and
binary checksum: the trace's package version alone does not identify a build.

Hook/screen spans identify the process, OS, version and surface. Session/turn/tool
identifiers are hashed correlation tokens, not raw payloads or a cryptographic
anonymization guarantee. Only known provider/identity-source labels are printed;
messages, status text, cwd, terminal text and titles are excluded. CLI routing
records candidate/selected surface IDs without logging the input payload.

`identity_source` describes presence ownership; `evidence` describes the input
being evaluated. `previous`/`next` describe the actual surface status, not the
workspace aggregate. `changed=false` is not necessarily rejection: repeated
reports and ledger-only events can be valid without changing the display.
Guard reasons distinguish stale sequence/turn, mismatched identity, ended
session, pending waits, active children, unchanged completion and settled-turn
spinner. `report_result`/`screen_result` record the underlying merge outcome;
an unchanged merge does not claim to identify a more specific core rejection.

Reproduce completion with and without Stop, both focused and hidden, while a
second session remains working. Then begin another turn and replay stale output.
`scripts/test-agent-hooks-gui.py` runs these cases through the actual CLI, IPC,
PTY and VTE. Never add a focus change just to make hidden completion pass.
