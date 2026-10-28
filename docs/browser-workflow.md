<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
# Browser tasks and action results

Flowmux can diagnose the current browser environment, reserve a tab for a local
agent task, hand control to the user, and report an action's observed outcome.
These features use the existing Linux WebKitGTK and macOS WKWebView panes.

## Choose an observation scope

For a known target, use `find PANE --role button --name Save --exact --compact --max-chars 6000`.
For an unfamiliar visible page, start with
`snapshot PANE --compact --viewport --max-nodes 40 --max-chars 12000`. Add `--interactive`
only when looking for controls; omit it for reading prose. Use `--within REF`
to inspect a known region. A filtered snapshot does not represent the whole page.
Check `truncated` before assuming there are no more matches, and enlarge the
scope or budget only when necessary. Every snapshot/find replaces the ref set.

`snapshot` and `find` accept `--compact` for model-facing JSON. It omits the
repeated Markdown tree and each element's selector, while retaining complete ref
tokens, page text, surface identity, truncation, states, descriptions and frame/
shadow paths. Selectors remain in the GUI's ref registry; actions and stale-ref
checks are unchanged. Omit `--compact` for the existing full format. `--json`
still wraps either format in `browser_result.value`. Compact output cannot be
combined with `--receipt`; it does not change snapshot limits or scope.

Prefer typed waits or an action's expected outcome to repeated full snapshots.
Wait polling runs inside Flowmux and returns one result. Use `form` for a known
set of fields. Keep large JSON in local files and expose only task-relevant data
to the model, preserving complete refs, surface identity and truncation flags.
These practices reduce returned text, not necessarily billed tokens by the same
percentage; model context caching and tool-output limits differ by agent.

## Wait for a new document

`navigate` and `reload` return after requesting navigation. A URL or
`ready-state=complete` wait can still match the previous document, especially
when reloading or navigating to the same URL. A receipt's document generation
advances when navigation starts, not when the new document finishes loading.

Prefer a site-specific changed result when available, such as a `TextChanged`
receipt expectation. For a full-document reload with no distinct result, mark
the current document with an unused temporary window property, then wait for
its disappearance and readiness together:

```sh
flowmux browser --surface "$SURFACE" eval "$PANE" \
  'window.__flowmuxNavigationPending = true; "marked"'
flowmux browser --surface "$SURFACE" reload "$PANE"
[ "$(flowmux browser --surface "$SURFACE" wait "$PANE" --js \
  "typeof window.__flowmuxNavigationPending === 'undefined' && document.readyState === 'complete'")" = true ] || exit 1
```

The same sequence works with `navigate "$PANE" "$URL"` in place of `reload`.
Then verify the expected URL and application result, and take a fresh snapshot
before using refs. Same-document SPA or fragment navigation does not replace the
window property; use the application's changed state for those actions instead.
JavaScript predicates belong to `wait --js`; receipt expectations do not accept
JavaScript. A timeout does not retry the navigation: inspect the state or keep
waiting without issuing the action again.

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

- X11 key/pointer injection requires the X11/XInput/XTest libraries.
- Wayland key/pointer injection uses the desktop's RemoteDesktop portal. The
  support check runs without opening a permission dialog. `wayland_portal_checking`
  means the check is pending; `wayland_portal_unavailable` means the desktop does
  not expose both keyboard and pointer control. `native_input_error` includes the
  cause. Once `wayland_permission_required` appears, run an input command to
  request permission, approve remote interaction in the desktop dialog and retry.
  `wayland_permission_pending` means answer the existing dialog;
  `wayland_portal_failed` reports denial, revocation or a disconnected portal.
  An explicit input retry rechecks an unavailable backend. A denied request
  never replays the original input. No permission or restore token is persisted.
  This permission covers desktop control, not just one browser tab. Flowmux
  checks that its window has focus; pointer commands also require the pointer
  to be over that window. Keep the window focused and avoid simultaneous manual
  input until the command finishes. The portal backend must support keyboard
  and pointer notifications (for example, xdg-desktop-portal-gnome).
  xdg-desktop-portal-wlr alone does not provide RemoteDesktop. Installing a
  file-chooser or screen-sharing portal does not imply native input support.
  Release held modifiers/buttons and turn off Caps Lock before key input;
  Flowmux rejects these conditions without changing the user's lock state.
  `type` uses native text editing and does not need this permission.
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
flowmux browser --surface "$SURFACE" --task "$TASK" snapshot "$PANE" --viewport --max-chars 12000
flowmux browser --surface "$SURFACE" --task "$TASK" fill "$PANE" "$REF" "value"
flowmux --json browser --surface "$SURFACE" --task "$TASK" control "$PANE" handoff
```

The browser displays the task state and the applicable **Take control**, **Continue**,
and **Stop** buttons. **Clear task display** is available in the details panel after the task ends. Handoff shows that user input is
needed. **Take control** pauses automation so the user can log in or edit the
page. Only the user-facing **Continue** button resumes the task; a client cannot
resume it through the control command. Take a fresh snapshot after continuing.

`control PANE status` is available without a task ID. The owning task can request
`handoff`, `pause`, `stop`, or `finish`. `finish` requires a running task with no
action in progress; it is an explicit completion declaration, not a site-result
check. Confirm the intended outcome before finishing. After stopping or finishing, **Clear task display** releases the tab
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
The receipt's `control` contains the current task summary without `results`;
previous result cards are not retransmitted on every action. Read
`control PANE status` to fetch the bounded history, including the latest card.
Clients that previously read `receipt.control.results` must use that command.
The UI result cards, task details, error fields and event truncation are unchanged.

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
Synchronous JavaScript completions have a 30-second host deadline, and asynchronous
completions have a five-minute ceiling. A stalled renderer cannot retain the
caller indefinitely. These deadlines do not stop page code or undo its effects;
inspect the page before retrying.

Use `download list` until the matching ID reaches `complete` before using its
file. Neither a download-start event nor a successful file-picker selection
proves completion of a transfer.

## Regression checks

`scripts/test-browser-control.py` uses a separate app, state directory and short
runtime path. It verifies ownership, handoff cancellation, receipts, timeout
non-repetition, popup/dialog events, active/inactive diagnostics and missing
ffmpeg, followed by existing browser-control scenarios.

The P1 regressions cover native text opening a confirm dialog, subsequent input
when a modal consumes keyup, and fixed-element capture after scrolling on both
platforms. Linux CI also enables
`FLOWMUX_TEST_X11_LOCKS=1` on its private Xvfb display to check logical key case
and restoration of Caps Lock. Do not enable that lock test on a user's display.

On Linux with GNOME Shell, xdg-desktop-portal-gnome, Xvfb, xwininfo, xdotool and
ffmpeg installed, run `python3 -B scripts/test-browser-wayland.py target/debug/flowmux`.
It creates its own nested compositor, D-Bus session and state directories,
declines and grants permission in that test desktop, checks trusted input, and
runs the browser-control scenarios without a post-picker refocus workaround.
It checks native picker selection/cancel, early CLI completion, backend request
cleanup and a portal configured without RemoteDesktop. It never clicks consent
on the user's desktop. This runner covers GNOME; KDE, wlroots and macOS need
separate native runs on those platforms.

## Find candidates and limit snapshots

`find` and `snapshot` accept `--role`, `--name`, `--label`, `--text`,
`--placeholder`, `--exact`, `--within REF`, `--interactive`, `--viewport`, `--max-nodes` and
`--max-chars`. Filters combine with AND. Role is exact; other strings use
case-sensitive, whitespace-normalized substring matching unless `--exact` is
set. `--within` uses a still-valid ref from the preceding snapshot. Each call
replaces the tab's ref registry, including when no candidates match. Multiple
matches remain multiple candidates; Flowmux never silently clicks the first.

```sh
flowmux --json browser find "$PANE" --role button --name '저장' --exact
flowmux --json browser find "$PANE" --label Email --placeholder Address
flowmux --json browser snapshot "$PANE" --within "$FORM_REF" --interactive --max-nodes 50 --max-chars 16000
```

Refs include visible, enabled/disabled, readonly, checked, selected, expanded,
editable, focused and invalid states, plus available ARIA descriptions. These are DOM observations, not a complete accessibility-tree API or a
promise that the target is unobstructed. Snapshots retain open shadow-root and
same-origin frame traversal. Closed shadow roots and cross-origin frames remain
unavailable. Text filters can return ancestor and descendant candidates.

Defaults are 2,000 refs and 262,144 serialized JavaScript character units; the
limits are 1–2,000 refs and 1,024–262,144 characters. The server adds the surface
identifier afterward. `truncated` reports omitted refs or summary text; refine
the scope/filter before concluding that an element is absent. Page URL/title can
also be shortened when the requested budget cannot fit metadata alone; use the
separate `url`/`title` commands when complete metadata is required.

## Wait before input and confirm typed outcomes

Click, double-click, hover and drag check visibility, enabled state where
applicable, hit testing and stable geometry for up to five seconds before native
input. Fill/select/check/uncheck wait up to two seconds for visibility and enabled state;
fill also waits for editable state. The original node identity must remain valid.
A replacement node is rejected even if it has the same selector. Preparation may
scroll the target into view. Only preparation is polled; an input or submission
is never automatically repeated. Native focus/platform requirements still apply.

Use `--expect JSON` with `--receipt`, or `wait --condition JSON`, for typed
observations without page-evaluated predicates:

```sh
flowmux --json browser --receipt --expect '{"TextChanged":{"selector":"#result"}}' click "$PANE" "$SUBMIT_REF"
flowmux browser wait "$PANE" --condition '{"UrlExact":"https://example.com/done"}'
flowmux browser wait "$PANE" --condition '{"ElementState":{"selector":"#submit","state":"enabled"}}'
flowmux browser wait "$PANE" --condition '{"Value":{"selector":"#email","value":"agent@example.test"}}'
flowmux browser wait "$PANE" --condition '{"DownloadComplete":{"id":0}}'
flowmux --json browser --receipt --expect '{"ValueChanged":{"selector":"#email"}}' fill "$PANE" "$EMAIL_REF" "agent@example.test"
flowmux --json browser --receipt --expect '{"PopupReady":{"url":"https://example.com/login"}}' click "$PANE" "$LOGIN_REF"
```

Element states are `visible`, `hidden`, `enabled`, `disabled`, `checked`,
`unchecked`, `selected`, `expanded`, `editable`, `focused`, and `invalid`.
Selectors in element conditions refer to the top-level document. `TextChanged`
and `ValueChanged`
capture the original text/value before the action (or before a standalone wait);
already-present text cannot satisfy a change. A supplied `before` is replaced by
the observed baseline. Other conditions intentionally assert current state and
can already be true. Failed/cancelled downloads return an error; timeout returns
false. `PopupReady` observes only popups created after the baseline and requires
the exact expected URL and complete readiness, so an initial blank document
cannot confirm navigation to a different URL. The receipt includes the new
surface ID. For an already-created popup, use its surface ID from the receipt and `--surface ID wait PANE
--ready-state complete` to check that specific child; popup creation alone is
not readiness. Observations do not prove server-side persistence or causality.

## Local trust, uploads and profile compatibility

Task IDs are public coordination identifiers, not authentication credentials.
IPC authenticates the OS peer UID in both directions and rejects other users. Socket and lock files
use mode 0600; lock files must be regular, single-link files and cannot be
symlinks. Runtime directories must be owned by the current user and not world-writable, or root-owned sticky temporary
directories. Newly created runtime directories use mode 0700. Group-writable
user-owned directories remain compatible; peer UID checks also reject a
substituted server owned by another user. Malformed request bodies
and parser-provided values are not logged.

File selection explicitly names the directory scope:

```sh
flowmux browser file-chooser "$PANE" select "$CHOOSER_ID" ./reports/result.csv --root ./reports
```

The GUI resolves canonical paths and rejects paths outside that root, including
symlink escapes. Folder uploads reject nested links/special files and trees over
10,000 entries; the native picker remains available. Invalid selections leave
the chooser pending. The chooser status includes its source label. macOS uses
the requesting frame origin; Linux marks that frame origin unavailable and shows
the top-level origin. Permission dialogs include scheme/host/port when exposed
by the backend, use a deny default, and omit URL credentials/path/query.

On Linux, Flowmux owns the file-chooser portal request and waits for its Close
acknowledgement before completing a CLI selection or cancellation. This handles
the GTK 4.14/GNOME race where cancellation reaches a backend before it exports the
picker request. Only that failed Close is retried; selecting files is not replayed.
A close failure returns an error instead of delivering the automated selection.
When no file-chooser portal is available, the GTK picker remains the fallback.

The directory scope is an explicit local-client grant, not a sandbox against
malicious programs already running as the same user. Such programs can choose a
broader root, modify files after validation or use raw eval/other OS APIs. No
capability token or task ID is presented as protection against that threat.
`value` and `attr value` reject password fields; raw eval, screenshots, arbitrary
page text and other attributes can still expose sensitive data. Avoid placing
secrets in command arguments and share captured output deliberately.

Linux profile directories use mode 0700 and reject a symlink as the profile
store. Custom profile keys now losslessly encode UTF-8 bytes and preserve case and
punctuation. Built-in profiles and existing canonical lowercase ASCII keys keep
their locations. Names whose old normalization lost information use new stores;
old stores/bookmarks are retained, never automatically merged or deleted. Those
profiles require a new login. Historical shared stores cannot be automatically
separated or assigned to an account; inspect them explicitly before manual
recovery. Profile names still determine storage identity, so renaming is not a
storage-preserving operation. Excessively long keys can exceed filesystem limits.

On Linux, persistent storage failure opens a separately isolated ephemeral
session with a visible warning and `diagnose.storage_ephemeral=true`; it never
uses the global default session. Cookie import returns `unimplemented` before
reading host cookies, because WebView insertion is not implemented. Sign in
inside the intended Flowmux profile.

## Task progress and result cards

After `control start`, describe the owning task with its ID:

```sh
flowmux browser --task "$TASK" --surface "$SURFACE" task "$PANE" \
  --name 'Contact report' --step 'Fill the form' \
  --waiting '' --user-action 'Review before submitting'
```

All four fields are optional; an empty string clears one field. Each is limited
to 500 characters. These are descriptions supplied by the task, not inferred
progress or proof of completion. Updating details requires the owning task ID
and does not resume a paused task. Use `control handoff` when the user must act.

The task bar shows a one-line summary and state-specific controls. **Details** /
**Results** opens a collapsible panel with the full task description and up to 20 recent action
results, related popup tabs, screenshots, uploads and tracked downloads. Related
tabs keep their own ownership. File buttons open saved captures and completed
downloads or their containing folders. Missing files cannot be opened. Results
and details are also returned by `control status`; they are runtime-only and
are cleared with **Clear task display**. Finishing the task does not mark an unverified
website action successful.

A selected upload is shown as **Files selected — upload unconfirmed**. To observe
a site's completion condition, use `file-chooser select --receipt --expect JSON`.
The result card then says **Upload condition observed** only if that condition
was met; it retains the selected filenames and request source. File selection
alone is not proof of a server upload. A page may upload immediately on selection.
Downloads update from native callbacks and only expose an open-file action after
completion. No command input values or raw receipt values are copied into cards.

## Form fields without a submit command

Use current, unambiguous snapshot refs from `find`/`snapshot`:

```sh
flowmux browser --task "$TASK" --surface "$SURFACE" form "$PANE" \
  '[{"target":"e1-SNAPSHOT","action":{"fill":"person@example.test"}},
    {"target":"e2-SNAPSHOT","action":{"select":"Work"}},
    {"target":"e3-SNAPSHOT","action":{"check":true}}]'
```

Replace the example refs with actual returned tokens. A form accepts 1–20 unique
targets, with `fill`, `select`, or `check` actions. Each field uses the existing
preparation and ref validation, executes once and checks its retained value and
HTML/ARIA validity. `select` accepts an option value or label, as the individual
command does. `fill` supports text inputs and textareas; native rich-editor
editing remains a separate `type` operation. Password fields require a separate
manual or explicitly chosen secret-input workflow and are rejected by `form`.

The response contains a `fields` array with `verified`, `invalid`, `failed`, `dialog_pending`, or
`not_run` for each target and `submit_command_sent: false`. Failed fields do not
undo earlier changes. Ordinary field failures allow later fields to run; handoff,
navigation, a pending dialog, an input timeout, or the 20-second batch deadline
stops remaining fields. Correct
only the failed/unexecuted fields using fresh refs if the page changed. There is
no automatic retry or submit step. Page input/change handlers can themselves send
requests, so this is not a transaction or a promise of no site-side effects.

A `dialog_pending` field has received input but has not been verified. Answer
the dialog with `browser dialog`, inspect the field's resulting value, and then
continue any `not_run` fields. Do not automatically repeat the pending field.

## Outcomes inside frames and open shadow roots

Use `Element` to bind an expectation to a current ref rather than a top-level
CSS selector:

```sh
flowmux browser wait "$PANE" --condition \
  '{"Element":{"target":"e4-SNAPSHOT","expect":{"Value":"updated"}}}'
flowmux browser --receipt --expect \
  '{"Element":{"target":"e4-SNAPSHOT","expect":{"ValueChanged":{}}}}' \
  fill "$PANE" e4-SNAPSHOT updated
```

`expect` accepts `State`, `Value`, `Text` (exact text), `ValueChanged`, or
`TextChanged`. State names match the existing `ElementState` conditions. Change
baselines are captured before the action/wait; caller-supplied baselines are
replaced. The target's original node, root and frame documents must remain valid
on every poll. Frame navigation, node replacement or a new snapshot invalidates
the ref even when its old selector matches another node. Conditions run in the
same isolated world as the snapshot. Closed shadow roots and inaccessible frames
remain unsupported. Existing selector conditions retain their top-level behavior.

## Browser toolbar

The default start page uses the pane theme's background with a centered Flowmux
icon. The address field is empty; the internal URL returned by `url` and snapshots
remains `about:blank`. Wait for the intended URL and page result before treating
navigation as complete. The start page is also retained when reloaded.

The toolbar keeps navigation, the address, bookmarks and downloads within reach.
**Browser tools** (the three-dot menu) contains **Find in page**, page zoom,
**Developer tools**, and the current profile/storage information. At narrow pane
widths, Bookmarks and Downloads move into this menu. Back, Forward and Reload
remain beside the address field. A non-default zoom
appears beside the address; click it to reset to 100%.

**Developer tools** opens Web Inspector in a separate window. On macOS, this keeps
the native inspector from resizing the GTK workspace and covering other panes.
Docking requests in the macOS inspector return it to the separate window.

Task details open to the right in wide panes and below the page in narrow panes.
Closing this panel keeps the task and results; **Clear task display** clears an
ended task's display without deleting downloaded files. New results never open
the panel automatically. Results remain session-only; this is not an archive.
