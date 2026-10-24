<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Browser tasks and action results

Flowmux can diagnose the current browser environment, reserve a tab for a local
agent task, hand control to the user, and report an action's observed outcome.
These features use the existing Linux WebKitGTK and macOS WKWebView panes.

## Check the environment

```sh
flowmux --json browser diagnose "$PANE"
flowmux --json browser --surface "$SURFACE" diagnose "$PANE"
```

The report includes the backend, target surface, active/visible state, pending
page dialog, input requirements, document-capture limits, recording encoder
availability, task state and recent native events. `environment_ready` only
checks environment prerequisites. Input still needs an actionable target or a
focused editable element.

- Linux key/pointer injection requires X11 and the X11/XInput/XTest libraries.
  Wayland reports `requires_x11`. Native text editing remains available on
  Wayland, but this does not enable key or pointer injection.
- macOS native input requires the visible active browser tab. Document capture
  depends on the installed WebKit snapshot API; older systems can use viewport
  capture.
- Recording checks the CLI's PATH for ffmpeg with libx264. Captures are silent
  and can omit accelerated layers. Playback of a particular media format must
  still be tested with that media file.
- Cookie extraction is separate from insertion into a WebView. Host-cookie
  insertion is not implemented; sign in inside the selected Flowmux profile.

`flowmux capabilities` remains an offline command inventory.

## Reserve and hand off a tab

Start a task and retain both IDs from the JSON result:

```sh
flowmux --json browser control "$PANE" start
# browser_result.value contains task_id and surface_id.
flowmux --json browser --surface "$SURFACE" --task "$TASK" snapshot "$PANE"
flowmux browser --surface "$SURFACE" --task "$TASK" fill "$PANE" "$REF" "value"
flowmux --json browser --surface "$SURFACE" --task "$TASK" control "$PANE" handoff
```

The browser displays the task state and **Take control**, **Continue**,
**Stop task**, and **Clear task** buttons. Handoff shows that user input is
needed. **Take control** pauses automation so the user can log in or edit the
page. Only the user-facing **Continue** button resumes the task; a client cannot
resume it through the control command. Take a fresh snapshot after continuing.

`control PANE status` is available without a task ID. The owning task can request
`handoff`, `pause`, `stop`, or `finish`. `finish` requires a running task with no
action in progress; it is an explicit completion declaration, not a site-result
check. Confirm the intended outcome before finishing. After stopping or finishing, **Clear task** releases the tab
for another task. Old task IDs remain invalid. Ending a task never closes its tab.

While a task owns a tab, modifications and snapshots require its task ID. A
snapshot updates the tab's reference registry, so competing snapshots are also
excluded. Read-only status, text/value queries and screenshots remain available.
Raw eval and JS wait predicates go through the same admission check as mutations.

Only one mutation runs per tab. Focus-affecting commands are additionally
serialized within the GUI process; a competing request fails promptly instead
of remaining queued for later execution. Handoff invalidates pending native
input preparation and stops subsequent JS-wait polling. A command already sent
to WebKit or the website may still finish. Stopping is not rollback.

Ownership is scoped to the chosen surface. New popup tabs have their own state;
they do not silently inherit the parent's task. These controls coordinate trusted
local clients and are **not an authentication boundary** against programs with
access to the user's IPC socket. They do not lock physical input or coordinate
focus across separate Flowmux processes. Use **Take control** before interacting
with the task's page.

## Confirm an action's result

Existing commands keep their existing response format. Add `--receipt` to get a
JSON action result. Optional expectations wait for text or a URL substring:

```sh
flowmux --json browser --surface "$SURFACE" --task "$TASK" \
  click "$PANE" "$REF" --receipt --expect-text "Saved" --outcome-timeout-ms 5000
flowmux --json browser --surface "$SURFACE" --task "$TASK" \
  press "$PANE" Enter --receipt --expect-url "/complete"
```

The receipt contains an action ID, surface ID, document generations before/after,
observed events, elapsed time, the original result/error and task state.

| Status | Meaning |
| --- | --- |
| `input_delivered` | The mutation returned; the intended site outcome was not checked. |
| `action_returned` | A read operation returned. |
| `outcome_confirmed` | The specified condition was observed after the action. |
| `dialog_pending` | Inspect `dialog status` and answer the dialog or hand off. |
| `popup_created` | A native popup creation event was observed; its surface ID is included. |
| `download_started` | A native download start was observed; completion is not implied. |
| `control_changed` | Control was handed off, stopped, cleared or the tab closed during the action. |
| `outcome_unknown` | The action errored or the expected condition was not confirmed in time. |

The event baseline is captured before dispatch. Native popup, download, dialog
and navigation callbacks record events even if a popup immediately closes.
Only the latest 64 events are retained in memory, and receipts flag truncation.
Document generations advance when native navigation starts and refs are
invalidated; a failed navigation can also advance the generation.

Events are observations during an action, not proof that the action caused each
event. Background page activity can also generate them. Events arriving after
the reply can be inspected in `diagnose`; an empty event list does not guarantee
that no later side effect will occur. Receipts do not store dialog messages,
credentials, page content or file contents in the event history. The original
command's returned value may itself contain sensitive data.

An outcome timeout **never repeats the action**. Inspect the current page before
retrying a submission or payment. `side_effects_possible` is conservative after
a mutation has been admitted, including when preparation later fails. Receipt
errors are carried inside the JSON result; consumers must inspect `status` and
`error`, not just the process exit code. Admission/protocol errors remain normal
CLI errors. `--outcome-timeout-ms` bounds the outcome wait, not the underlying
command; use `eval-async --timeout-ms` for bounded asynchronous evaluation.

Use `download list` until the matching ID reaches `complete` before using its
file. Neither a download-start event nor a successful file-picker selection
proves completion of a transfer.

## Regression checks

`scripts/test-browser-control.py` uses a separate app, state directory and short
runtime path. It verifies ownership, handoff cancellation, receipts, timeout
non-repetition, popup/dialog events, active/inactive diagnostics and missing
ffmpeg, followed by existing browser-control scenarios.

The P1 regressions cover native text opening a confirm dialog and fixed-element
capture after scrolling on both platforms. Linux CI also enables
`FLOWMUX_TEST_X11_LOCKS=1` on its private Xvfb display to check logical key case
and restoration of Caps Lock. Do not enable that lock test on a user's display.
