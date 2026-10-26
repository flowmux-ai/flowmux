---
name: flowmux-browser
description: Operate Flowmux's in-app browser, verify page actions, manage file transfers and captures, and control workspaces, terminals and SSH through its CLI. Use for Flowmux feature guidance or automation inside a Flowmux pane or with an explicitly supplied socket and pane.
---

# Flowmux CLI

Use the category matching the user's task. This skill describes product usage;
it does not install development tools or prescribe a software development process.

| Category | Tasks |
|---|---|
| [Context](#context) | Connect to the intended window; identify panes and tabs |
| [Workspaces and terminals](#workspaces-and-terminals) | Split, focus, resize, send input and read output |
| [Browser](#browser) | Inspect pages, interact, verify forms/results, transfer files and capture |
| [SSH](#ssh) | Connect, reconnect, forward ports and preview |
| [Agents and notifications](#agents-and-notifications) | Inspect live agents and report task completion |
| [Settings and integrations](#settings-and-integrations) | Themes, skills and diagnostics |
| [GUI features](#gui-features) | Code Review, search, Files, editor and AI Usage |

## Context

```bash
FLOWMUX_CLI="${FLOWMUX_BUNDLED_CLI_PATH:-flowmux}"
"$FLOWMUX_CLI" ping
"$FLOWMUX_CLI" --json identify
"$FLOWMUX_CLI" --json tree
"$FLOWMUX_CLI" --help
```

`FLOWMUX_PANE_ID` means the shell was launched inside Flowmux. `identify` reads
context; only a successful `ping` proves the GUI socket is reachable. If the
socket is stale, report it. Do not restart the user's app or silently select
another window. Outside Flowmux, use the user's supplied socket with the global
`--socket /path/to/socket` option **before** the command; use explicit pane IDs.

- `FLOWMUX_SOCKET_PATH`: window connection; different windows have different sockets.
- `FLOWMUX_WORKSPACE_ID` (`FLOWMUX_TAB_ID` alias): workspace UUID.
- `FLOWMUX_PANE_ID`: pane UUID; arguments accept bare UUID or `pane:<uuid>`.
- `FLOWMUX_SURFACE_ID`: tab UUID inside a pane; distinct from the pane ID.
- `FLOWMUX_BUNDLED_CLI_PATH`: optional matching CLI binary.

Read `tree` to select targets. In examples, `$PANE`, `$WORKSPACE`, `$SURFACE`
and `$FORWARD` are IDs from the corresponding command responses, not names.
Pane and surface IDs are not interchangeable: `PANE` is the containing pane;
`--surface UUID` selects one of its tabs. Do not use the calling terminal's
`FLOWMUX_SURFACE_ID` as a browser tab ID.

`--json` gives structured responses. `--version`, `capabilities` and command
`--help` describe the selected CLI, which may differ from the running GUI.
Use `browser diagnose PANE` for live backend support. If an option is unknown,
check that binary's help before choosing a supported equivalent; a new skill
does not upgrade the app, and an unsupported operation is not a reason to restart
the user's session.

When targeting another authorized window with `--socket`, inherited pane IDs
still refer to the original window. Read the target window's `tree` and use its
IDs. `browser open` has no pane argument: set `FLOWMUX_PANE_ID` to the chosen
source pane for that invocation, or unset it to use that window's focused pane.

## Workspaces and terminals

```bash
"$FLOWMUX_CLI" workspace ls
"$FLOWMUX_CLI" workspace current
"$FLOWMUX_CLI" workspace new --name Research --root /path/to/project
"$FLOWMUX_CLI" workspace focus "$WORKSPACE"
"$FLOWMUX_CLI" split "pane:$PANE" --right
"$FLOWMUX_CLI" split "pane:$PANE" --down
"$FLOWMUX_CLI" resize-pane "pane:$PANE" --ratio 0.5
"$FLOWMUX_CLI" focus-pane "pane:$PANE"
"$FLOWMUX_CLI" new-tab --workspace "$WORKSPACE" --cwd /path/to/project
"$FLOWMUX_CLI" focus-tab "$SURFACE" --pane "pane:$PANE"
```

`resize-pane` sets the parent split's first-child ratio. `new-tab` creates a
terminal tab; inspect the returned/tree IDs before operating on it.

To run a user-requested command in another terminal: inspect its screen, confirm
it is the intended shell, send text, send Enter, then read the result.

```bash
"$FLOWMUX_CLI" read-screen "pane:$PANE"
"$FLOWMUX_CLI" send-keys "pane:$PANE" 'pwd'
"$FLOWMUX_CLI" send-key Enter --pane "pane:$PANE"
"$FLOWMUX_CLI" read-screen "pane:$PANE"
```

`send-keys` requires an explicit target and accepts escapes; `send-key` sends a
named key to the terminal, **not an application UI shortcut**. `read-screen`
reads the visible terminal buffer, not all scrollback or browser/editor text.
Command submission does not prove completion; inspect its output. Avoid typing
into an active agent or interrupting an unrelated task.

Only close the user's intended target:

```bash
"$FLOWMUX_CLI" close-tab "$SURFACE" --pane "pane:$PANE"
"$FLOWMUX_CLI" close-pane "pane:$PANE"
```

The final workspace pane/tab is protected. Unsaved editor documents can prompt
for confirmation; a pending request is not a completed close. Do not bypass it.

## Browser

Inside Flowmux, prefer its visible in-app browser for web tasks. Do not launch
Playwright, Puppeteer or system Chromium merely to read a page.

The example uses `jq` to extract the opened pane ID. If unavailable, read the JSON
response directly instead of blindly installing dependencies.

```bash
OPEN=$("$FLOWMUX_CLI" --json browser open https://example.com) || exit 1
PANE=$(printf '%s' "$OPEN" | jq -er '.browser_pane_opened.pane') || exit 1
[ "$("$FLOWMUX_CLI" browser wait "pane:$PANE" --url example.com)" = true ] || exit 1
[ "$("$FLOWMUX_CLI" browser wait "pane:$PANE" --ready-state complete)" = true ] || exit 1
SNAPSHOT=$("$FLOWMUX_CLI" browser snapshot "pane:$PANE" --compact --viewport --max-nodes 40 --max-chars 12000) || exit 1
SURFACE=$(printf '%s' "$SNAPSHOT" | jq -er '.page.surface_id') || exit 1
printf '%s\n' "$SNAPSHOT"
# Pin subsequent operations to this browser tab.
# "$FLOWMUX_CLI" browser --surface "$SURFACE" ...
```

Open reuses a right-sibling browser pane as a new tab, or splits beside the
calling pane. The default start page has an empty address field, the pane theme's
background and a centered Flowmux icon; its internal URL is still `about:blank`.
Older builds may show a plain blank page and the literal address. Neither proves
the requested site loaded. Wait for the requested URL and readiness before taking
a snapshot. Wait prints `true` or `false`; **check the value**, not just the exit
code. Stop dependent actions on timeout and inspect the current URL/diagnostics.
`--url` matches a substring; use `--condition '{"UrlExact":"https://example.com/"}'`
when the exact URL matters. `complete` is document readiness, not completion of
later application requests; wait for the result that the task actually needs.

For `reload` or same-URL `navigate`, URL and ready-state waits can match the old
document. Use a site-specific changed result, or mark the current document with
an unused temporary window property before the action and use `wait --js` to
require both its disappearance and `document.readyState === 'complete'`.
Then verify the destination/result and refresh refs. This marker technique is
for full-document navigation, not SPA or fragment changes. Receipt document
generations indicate navigation start, not completed loading; receipt
expectations reject JavaScript. A timeout never means the action should be repeated.

Choose the smallest observation that answers the next question:

- Known target: `find PANE --role button --name Save --exact --compact --max-chars 6000`.
- Visible controls: `snapshot PANE --compact --viewport --interactive --max-nodes 40 --max-chars 12000`.
- Reading content: omit `--interactive`; it excludes document prose. Scope with
  `--within REF`, then read the required element with `text PANE REF`.
- Completion: use a typed `wait` or action `--receipt --expect` instead of
  repeatedly dumping snapshots. Wait polling stays inside Flowmux.
- Multiple form fields: use `form` and inspect its per-field results.

Check `truncated` before deciding something is absent. A viewport/filter also
excludes content even when `truncated` is false. Expand the scope or budget only
when needed. Plain browser output is already JSON for snapshots; `--json` adds
an outer response envelope whose `browser_result.value` is a JSON-encoded string
(`jq '.browser_result.value | fromjson'`). Do not add `--json` when parsing the
plain snapshot's `.page` or `.refs`. Keep large responses in local files and
return only relevant fields to the model, retaining complete refs and scope.
After navigation, node replacement or handoff, refresh the necessary scope;
never reuse an old ref just to save tokens. Every snapshot/find replaces refs.

Use `--compact` on `snapshot`/`find` to omit the duplicate Markdown and element
selectors from model-facing JSON. Complete refs, page text, states, descriptions
and frame/shadow paths remain. Actions still use the full server-side registry.
Omit it when a script needs the full format; `--receipt` cannot be combined with it.

A full snapshot returns a Markdown tree and opaque refs such as `e1-<snapshot-id>`.
Copy the entire returned ref; short names such as `e1` are not aliases.
Refs expire when their element is replaced, even if its selector stays the same.
Snapshot names prefer valid `aria-labelledby` references, then `aria-label`,
associated HTML labels and element text. Referenced hidden labels are included;
title and placeholder provide fallbacks. This is a DOM summary, not a complete
platform accessibility tree (for example, CSS-generated text is not included).
Snapshots include nested open Shadow DOM. For those refs, `shadow_path` lists
host selectors from outermost to innermost; `selector` is relative to the last
shadow root. Use the ref for actions, which validate the original node and root.
Closed shadow roots are not exposed.
Same-origin iframe contents are included, with `frame_path` listing each frame's
selector and optional shadow-host path. Child navigation or frame replacement
invalidates its refs. Native input follows the focused document; pointer targets
account for frame borders, scrolling and positive axis-aligned scaling. Rotated,
skewed or perspective-transformed frames reject pointer actions.
Frame elements expose `frame_accessible`; false means their contents cannot be
read in the current document (including cross-origin and opaque sandboxed
frames). Use refs inside accessible frames, not a click on the frame container.
Snapshot JSON includes `page.surface_id`. Pass `browser --surface UUID` before
any browser command except `open` to keep targeting that tab after another tab
becomes active. A closed tab or a surface outside the given pane returns an error.
Without this option, commands use the pane's active tab.
Actions reject hidden or inert elements. Clicks also reject disabled or covered
targets, and fill rejects disabled or readonly fields. Wait for the page to reach
the required state before retrying; commands do not retry side effects.
`type` uses native editing at the current selection, including contenteditable,
and reports canceled input. It emits native input events without artificial
per-character key events. On macOS the target tab must be visible.
`press` sends native keys to a visible tab on macOS, X11 and Wayland, including Enter,
Tab, Shift+Tab and Control+a (macOS: Meta+a). Page key handlers can cancel their
default action. Completion confirms delivery, so still check the expected page
state. Wayland first checks whether the desktop RemoteDesktop portal supports
keyboard and pointer input. Run `diagnose` until the check completes, then run an
input command to request permission. Approve remote interaction and explicitly retry.
The grant covers desktop control, not just this tab. Keep Flowmux focused and
the pointer over its window for pointer commands; avoid concurrent manual input.
Release held modifiers/buttons and turn off Caps Lock before Wayland key input.
Use `diagnose` to distinguish checking/unavailable portal support from required,
pending or failed permission; `native_input_error` gives the cause. Pending means
answer the existing desktop dialog. xdg-desktop-portal-wlr alone does not implement
RemoteDesktop; a file-chooser or screen-sharing portal is not sufficient. Use `type`
for Unicode text or characters missing from the current keyboard layout.
`click`, `dblclick` and `hover` also require the visible active tab. They wait
for two stable rendering frames, reject covered targets and send
native pointer events. `hover` activates CSS hover; double-click includes its
two clicks. A blocked or moving target returns an error without retrying.
Key and pointer completion waits for synchronous event handlers and native
default actions. If an input opens a page dialog, the command returns once the
dialog is available; answer it with `dialog` before checking the page result.
Use `wait` for later network responses or other asynchronous page changes.
Set `$BUTTON_REF`, `$FIELD_REF` and `$SELECT_REF` from the current snapshot's
complete tokens, such as `e3-<snapshot-id>`. These are independent examples;
refresh refs if an earlier action changed the page:

```bash
"$FLOWMUX_CLI" browser --surface "$SURFACE" click "pane:$PANE" "$BUTTON_REF"
"$FLOWMUX_CLI" browser --surface "$SURFACE" fill "pane:$PANE" "$FIELD_REF" "search terms"
"$FLOWMUX_CLI" browser --surface "$SURFACE" select "pane:$PANE" "$SELECT_REF" "option value"
"$FLOWMUX_CLI" browser --surface "$SURFACE" type "pane:$PANE" "text for the focused field"
"$FLOWMUX_CLI" browser --surface "$SURFACE" press "pane:$PANE" Enter
"$FLOWMUX_CLI" browser --surface "$SURFACE" is-visible "pane:$PANE" "$BUTTON_REF"
"$FLOWMUX_CLI" browser --surface "$SURFACE" text "pane:$PANE" "$BUTTON_REF"
"$FLOWMUX_CLI" browser --surface "$SURFACE" value "pane:$PANE" "$FIELD_REF"
"$FLOWMUX_CLI" browser --surface "$SURFACE" attr "pane:$PANE" "$BUTTON_REF" href
"$FLOWMUX_CLI" browser --surface "$SURFACE" count "pane:$PANE" ".result-row"
"$FLOWMUX_CLI" browser --surface "$SURFACE" screenshot "pane:$PANE" /tmp/flowmux-page.png
"$FLOWMUX_CLI" browser --surface "$SURFACE" screenshot "pane:$PANE" /tmp/flowmux-document.png --full-page
"$FLOWMUX_CLI" browser --surface "$SURFACE" screenshot "pane:$PANE" /tmp/flowmux-element.png --element "$BUTTON_REF"
```

### Captures and command reference

Screenshots default to the visible viewport. `--full-page` captures the document
without scrolling or resizing the page and rejects bitmaps above 64 MiB at the
current zoom and display scale. On macOS it requires WebKit's content-rectangle
snapshot support (macOS 15 or newer). Native document snapshots may omit video
and WebGL layers; they are not a recording of the composited screen.
`--element REF` crops the current viewport when the element is fully visible,
including open Shadow DOM and accessible nested frames. Offscreen elements use
a document capture with the limits above. Fixed or sticky elements, and elements
clipped by their frame, must be fully in view. Capture validates the ref and
does not change scrolling. `--element` and `--full-page` cannot be combined.

| Command family | Usage after `browser` |
|---|---|
| Navigation | `navigate PANE URL`, `back PANE`, `forward PANE`, `reload PANE` |
| Page metadata | `url PANE`, `title PANE` |
| Element interaction | `dblclick`, `hover`, `focus`, `blur`, `check`, `uncheck`: each takes `PANE REF` |
| State | `is-enabled PANE REF`, `is-checked PANE REF` |
| Live diagnostics | `diagnose PANE` |
| Task control | `control PANE start`, `status`, `handoff`, `pause`, `stop`, or `finish`; Continue is a user UI action |
| Task details | `task PANE --name NAME --step STEP --waiting REASON --user-action ACTION` with owning `--task` |
| Form fields | `form PANE JSON` with 1–20 unique current refs and fill/select/check actions; no submit command |
| Media | `media PANE REF status`, `play`, `pause`, `seek SECONDS`, `mute`, or `unmute` |
| Downloads | `download PANE list`, `cancel ID`, or `directory EXISTING_PATH` |
| Page dialogs | `dialog PANE status`, `accept ID [--text TEXT]`, or `dismiss ID` |
| File selection | `file-chooser PANE status`, `select ID PATH... --root DIR`, or `cancel ID` |
| Coordinates | `pointer PANE click\|dblclick\|hover X Y`, `drag PANE FROM_X FROM_Y TO_X TO_Y` |
| Scrolling | `scroll PANE REF X Y`: bring the element into view, then offset the viewport |
| Wait | `wait PANE --condition JSON`, `--selector CSS`, `--text TEXT`, `--url TEXT`, `--ready-state complete`, or `--js PREDICATE`; optional `--timeout-ms` / `--poll-ms`; no network-idle wait |
| Video capture | `record PANE output.mp4 --seconds 5 --fps 5` |
| Escape hatch | `eval PANE JAVASCRIPT` when normal verbs cannot express the task |
| Async completion | `eval-async PANE BODY --timeout-ms 10000`; BODY is a function body, e.g. `return await fetch('/status').then(r => r.text())` |

`eval-async` waits for the returned Promise and reports rejection or timeout.
A timeout stops waiting, but cannot cancel page effects; inspect state before
retrying a command that changes the page.

### Media, files and popups

Audio and video elements with visible bounds appear in snapshots. `media`
returns JSON with `paused`, `ended`, `seeking`, `muted`, `current_time`,
`duration` (null if unknown or unbounded), `ready_state`, `network_state`, and
`error`. Play waits for playback time to advance; seek rejects unavailable
ranges and waits for seeking to finish. Commands report codec, playback-policy
and timeout errors instead of bypassing browser restrictions. After a timeout,
inspect `status` before retrying. Linux playback needs the GStreamer codecs
described in `docs/setup.md`.

`dialog status` returns `{"dialog": null}` when no page alert, confirm, or
prompt is pending. Otherwise `dialog` contains its `id`, `kind`, `message`, and
`default_text` (the prompt's initial value, otherwise null). Copy that ID into
`accept` or `dismiss`; a stale ID cannot answer a subsequent dialog. `--text`
is valid only for prompts; omit it to keep the current entry text. Commands
wait for the panel to be ready and fully close before succeeding. After a
timeout, inspect `status` before retrying because the page may already have
received its answer. Use `--surface` for dialogs belonging to an inactive tab.
These commands handle page JavaScript dialogs, not permission requests or
before-unload confirmation. The visible panel still accepts normal user input.

Activate a file input with `click REF`, or `focus REF` then `press Enter`.
`pointer PANE click|dblclick|hover X Y` uses CSS pixels from the top-left of the
visible browser viewport, without scrolling. It supports canvases, open shadow
roots and accessible frames, and requires the active tab. Invalid coordinates,
inert/hidden targets and inaccessible cross-origin frames are rejected before
input. Use `--surface` to pin the tab, and inspect the page again after scrolling
or resizing before choosing new coordinates.

`drag PANE FROM_X FROM_Y TO_X TO_Y` holds the left button while moving along a
straight path between two viewport points, then releases it. Both endpoints
must remain stable before input starts. Canvas pointer capture is supported;
coordinates, tab visibility and native overlays are checked before pressing.
Afterward inspect the page to confirm the application's intended result.

`record PANE output.mp4 --seconds 5 --fps 5` samples the viewport into a silent
H.264 MP4. It requires `ffmpeg` with `libx264` on PATH, pins the initial browser
tab (or `--surface`), and refuses to replace an existing file. The command runs
in the foreground; Ctrl-C cancels it and removes temporary frames. Duration is
1–60 seconds and sampling is 1–10 fps, with actual capture timing preserved.
The first frame fixes the video dimensions; resized viewports fit inside that
canvas with their aspect ratio preserved. The temporary frame budget is 256 MiB.
This uses the same
snapshot path as PNG capture, so hardware video/WebGL layers may be omitted.

Page-created popups open as browser tabs in the source pane. They retain their
engine-created window context, including `opener`, named-window reuse and
`postMessage`; `noopener` still isolates the child. Use a fresh snapshot and
`--surface` to address the parent or child explicitly. `window.close()` closes
the child tab, and closing a child preserves the parent's page state.

`file-chooser status` returns `{"file_chooser": null}` when none is pending;
otherwise it returns an `id`, `multiple`, and `directories` flag. Use that ID
with `select ID PATH... --root DIR` to provide existing readable paths within
an explicit directory scope, or `cancel` to preserve the
page's previous selection. Relative paths are resolved from the CLI's working
directory. The selection must match the native request's count and file/folder
type. Invalid paths and stale IDs leave the pending chooser untouched. Selection
makes those files available to the requesting page; verify its change event or
upload result afterward. The standard file picker also remains usable manually.
Use `--surface` to keep responses on the intended browser tab.

`download` returns JSON with the tab's `directory` and ordered `downloads`.
Each item has an `id`, `state` (`in_progress`, `cancelling`, `complete`,
`cancelled`, or `failed`), destination `path` (null until selected), `progress`
from 0 to 1, and `error` (null unless failed). Poll `list` for a terminal state;
a successful `cancel` request alone does not mean cancellation has completed.
IDs and directory settings belong to that browser tab; use `--surface` to keep
commands on it when the active tab changes. `directory` resolves relative paths
against the CLI's working directory and requires an existing folder. It affects
subsequent downloads until the tab closes, without changing other tabs or the
system default. Existing files are not overwritten. Cancelling does not promise
to remove partial files; only `complete` means a successful download. The UI and
CLI share the same history; cleared or evicted entries are no longer listed.

### Forms and result verification

The control bar shows a one-line task summary. Details / Results opens a
collapsible panel containing the full description, related tabs, saved
captures, upload observations and native download completion. `control status`
returns these bounded runtime-only results. File selection is not upload success;
use `file-chooser select --receipt --expect JSON` for a site completion condition.
`form PANE '[{"target":"FULL_REF","action":{"fill":"value"}}]'` returns a
`fields` array whose statuses are `verified`, `invalid`, `failed`, or `not_run`,
and `submit_command_sent: false`. Actions are `{"fill":"value"}`,
`{"select":"option value or label"}`, or `{"check":true}` / `{"check":false}`.
Inspect every field: returning JSON does not mean all fields passed. Earlier
changes are not rolled back when a later field fails, and page input/change
handlers can themselves send requests. Retry only failed/unexecuted fields after
inspecting the page. Password fields are rejected by the form command; `type`
is the native editing path for a focused rich editor.
Use `wait --condition '{"Element":{"target":"FULL_REF","expect":{"Value":"value"}}}'`
for same-origin iframe/open shadow targets. `State`, `Text`, `ValueChanged` and
`TextChanged` are also supported. Replaced nodes/frames fail instead of matching
another node. Existing CSS selector conditions target the top-level document.

Never add tracking attributes to the DOM; Flowmux resolves refs without modifying
the page. WebKitGTK/WKWebView do not expose CDP. Device viewport emulation, network
mocking, full-page tracing and CDP screencast are unavailable; `record` provides
sampled MP4 capture, and `screenshot --full-page` provides a document PNG. Use
those supported commands when they meet the task. For a strict CDP requirement,
explain the missing capability and keep user-visible URLs/results in Flowmux
alongside the external tool. Cookie import returns `unimplemented` without
reading host cookies; sign in inside the intended Flowmux profile.

Linux and macOS offer opt-in WebDriver sessions in separate ephemeral
Flowmux browser windows. Linux requires a WebKitGTK runtime built with WebDriver
input enabled; macOS starts with `FLOWMUX_WEBDRIVER_SERVER=127.0.0.1:4444 flowmux`. These are WebDriver targets, not CLI pane targets; existing pane
tabs and CLI input commands are unchanged. See `docs/webkit-automation.md` in the
Flowmux repository for setup and the native-input smoke test. There is no CDP
attachment. The macOS endpoint supports a documented subset of WebDriver.

### Runtime browser diagnostics

`flowmux --json browser diagnose PANE` reports the actual backend, visible/active
surface, pending dialog, native input environment, document capture limits and
recording encoder availability. Use `--surface UUID` to check a specific tab.
`environment_ready` checks the environment only; the target must still be
editable/actionable. Recording checks the CLI's PATH for ffmpeg/libx264. Media
codec support still requires playing the intended file. `capabilities` remains
an offline command inventory.

### Task ownership and action receipts

Use `browser control PANE start` to reserve one tab; retain the returned `task_id`
and `surface_id`. Pass `--task ID --surface UUID` on subsequent mutations and
snapshots/finds (they replace refs). Raw eval and JS waits also require ownership.
Read-only status, text/value queries and screenshots remain available.
`control PANE handoff` or `pause` blocks new mutations. The user resumes
with **Continue** in the browser task bar. `stop` and `finish` preserve the tab;
**Clear task display** releases it. `control PANE status` remains readable without an ID.
Take a fresh snapshot after the user continues. These controls coordinate local
clients; they are not a sandbox or credential boundary.

```bash
CONTROL=$("$FLOWMUX_CLI" browser --surface "$SURFACE" control "pane:$PANE" start) || exit 1
TASK=$(printf '%s' "$CONTROL" | jq -er '.task_id') || exit 1
"$FLOWMUX_CLI" browser --surface "$SURFACE" --task "$TASK" task "pane:$PANE" \
  --name "Requested task" --step "Inspect the page"
# All later mutations and snapshots/finds include the owning task ID.
"$FLOWMUX_CLI" browser --surface "$SURFACE" --task "$TASK" find "pane:$PANE" \
  --role button --name Save --exact --compact --max-chars 6000
# After verifying the requested result:
"$FLOWMUX_CLI" browser --surface "$SURFACE" --task "$TASK" control "pane:$PANE" finish
```

`finish` requires a running task with no action in progress; it declares completion
and does not verify the website. A new task cannot start until the ended task's
display is cleared in the UI. Popup tabs have independent ownership. Handoff or
stop can cancel pending preparation but cannot undo an already-delivered action.

Add `--receipt` to an action, optionally `--expect-text TEXT` or `--expect-url URL`
or `--expect JSON`, and `--outcome-timeout-ms 5000`. Choose one expectation form.
Inspect the returned JSON `status` and `error`, even when the CLI exits zero:
`input_delivered` is not an application-level outcome; `outcome_confirmed` means
the condition was observed; `dialog_pending` requires a response; and
`outcome_unknown` requires inspection before retrying. The command is never
repeated automatically. Receipt `control` is a task summary without the `results`
history; use `control PANE status` when the result cards are needed, rather than
after every action. Popup/download events include their IDs, but a download
must separately reach `complete`. `popup_created` and `download_started` are
intermediate outcomes; `control_changed` requires inspecting the task state.
`--outcome-timeout-ms` bounds the outcome wait, not the underlying command.
See `docs/browser-workflow.md` in the source
repository for the protocol, event-history limits and handoff semantics.


### Semantic discovery and typed conditions

`find` is an alias of `snapshot`, not a click or a unique-element assertion.
The role/name/label/text/placeholder filters combine with AND. Role is exact;
other text uses case-sensitive, whitespace-normalized substring matching unless
`--exact` is set. Multiple candidates require choosing the intended match.
Even zero matches replace the ref registry. `--within REF` must come from the
preceding still-valid snapshot; read returned refs before the next action.

Pointer actions wait up to five seconds for readiness before input; fill/select/
check/uncheck wait for their applicable element states. Stale nodes still fail.
An action is never automatically replayed after delivery.

Use `--receipt --expect '{"TextChanged":{"selector":"#result"}}'` to require a
change from the pre-action baseline. `wait --condition JSON` and `--expect JSON`
also accept `{"ValueChanged":{"selector":"#email"}}`,
`{"PopupReady":{"url":"https://example.com/login"}}`, `{"UrlExact":"https://example.com/done"}`,
`{"ElementState":{"selector":"#submit","state":"enabled"}}`,
`{"Value":{"selector":"#email","value":"agent@example.test"}}` and
`{"DownloadComplete":{"id":0}}`. These selectors address the top-level document.
Use the popup's explicit surface ID and a ready-state wait for child readiness.

`TextChanged` and `ValueChanged` capture their baseline before dispatch (or at the
start of a standalone wait); supplied `before` values are replaced. Attach them
to the action's receipt to detect that action's change. Starting a change wait
after the update can instead wait for a future update. `PopupReady` likewise
observes only newly created popups, requiring the exact URL and complete readiness.
For an existing child, wait on its explicit surface. Current-state conditions
can already be true; none of these checks alone proves server-side persistence.

File selection requires `--root DIR`; canonical paths must remain inside it.
Folder uploads reject nested links/special files and trees exceeding 10,000
entries. `value` and `attr value` reject password fields; raw eval and captures
remain trusted local-client capabilities. Task IDs are not authentication. IPC
accepts only the same OS user, and file scope is not a same-user sandbox.
Cookie import reports unsupported without reading or claiming to inject cookies.

## SSH

Run these commands from the local Flowmux context. Remote shells do not inherit
its pane/socket context. Replace the sample host and ports with the user's target.

```bash
"$FLOWMUX_CLI" ssh connect user@host --cwd /srv/project
"$FLOWMUX_CLI" ssh status --workspace "$WORKSPACE"
"$FLOWMUX_CLI" ssh reconnect --workspace "$WORKSPACE"
"$FLOWMUX_CLI" ssh forward add --workspace "$WORKSPACE" --remote-port 3000 --local-port 3000
"$FLOWMUX_CLI" ssh forward list --workspace "$WORKSPACE"
"$FLOWMUX_CLI" ssh preview "$FORWARD" --workspace "$WORKSPACE"
"$FLOWMUX_CLI" ssh forward remove "$FORWARD" --workspace "$WORKSPACE"
"$FLOWMUX_CLI" ssh disconnect --workspace "$WORKSPACE"
```

Workspace creation does not prove SSH authentication succeeded: inspect `status`
and the authentication terminal. `connect` also accepts `--port`, `--user`,
`--identity-file`, `--config-file`, `--name`, `--tmux`, and a command after `--`.
That command runs once; reconnect does not replay it. `--tmux` requires remote
tmux; without persistence, disconnection can end remote processes.
`ssh preview` is Linux-only. Remote Files/editor/worktrees/project commands and
remote hook installation are not supported.
Code Review supports remote working changes and commit history through the
connected workspace's SSH session. Open it from the pane menu or `Ctrl+Alt+E`;
the remote host needs Git, and review comments are stored locally per SSH target.

## Agents and notifications

```bash
"$FLOWMUX_CLI" --json agents
"$FLOWMUX_CLI" session-name
"$FLOWMUX_CLI" notify --pane "pane:$PANE" --title "Task update" --level info "Ready for review"
"$FLOWMUX_CLI" notify-complete --agent Codex --pane "pane:$PANE" --message "Requested task finished"
"$FLOWMUX_CLI" --json notifications list --unread
"$FLOWMUX_CLI" notifications jump-to-unread
```

`agents` reports live agents; saved session history is separate. Claude session
names help map panes to sessions. If Claude's `ListAgents` and `SendMessage`
tools are available and the task calls for coordination, verify the name with
`ListAgents` before messaging; a user rename can make the mapping stale.
Notifications appear in Flowmux/desktop notification surfaces; they are not
messages to another agent. `notifications open ID` focuses the source pane,
`mark-read ID` marks without focusing, and `clear` removes all notifications.
Do not simulate lifecycle hooks to make an agent appear active.

`claude-teams --count N --root PATH` starts 1–8 Claude agents in panes. Use it
only when the user requests a team; listing agents does not require launching any.

## Settings and integrations

For users, open **Options → Skills** and click **Install** next to the agent they
use. The same tab offers **Update**, **Remove**, and **Refresh status**. Updates
and removal back up modified skill content and preserve agent hooks/settings.
**View skill contents** previews the bundled instructions. Agent skill lists use
its existing `flowmux-browser` identifier. A separate Codex copy shown in the row
can remain discoverable after removing this managed copy; do not delete user-owned
copies automatically. Each agent is managed separately. The equivalent CLI
commands are below.

```bash
"$FLOWMUX_CLI" theme path
"$FLOWMUX_CLI" theme import /path/to/example.theme
"$FLOWMUX_CLI" agent install --agent codex
"$FLOWMUX_CLI" --json agent doctor --agent codex
"$FLOWMUX_CLI" agent install --agent codex --force
"$FLOWMUX_CLI" agent uninstall --agent codex --skills-only
"$FLOWMUX_CLI" agent install --agent gemini --skill flowmux-team
"$FLOWMUX_CLI" agent doctor --agent agy --skill flowmux-team
"$FLOWMUX_CLI" doctor
```

Install targets: `claude-code`, `opencode`, `codex`, `antigravity` (`agy` alias),
`gemini`, `cline`. Omitting `--agent` selects every target supported by the selected
skill. `--skill flowmux-browser` is the default; `--skill flowmux-team` selects
the complete Team bundle for Claude Code, Codex, Gemini CLI or agy.
All four can lead teams; worker sessions currently use Claude Code or Codex.
This user guide retains the
`flowmux-browser` skill name/path for existing installations. Installation copies
the CLI's bundled guide; no external skill catalog or browser download is needed.
Identical content is unchanged. `--force` backs up modified content before update;
`--skills-only` removal preserves wrappers/hooks and backs up modified text.
After an in-app update, the new version's first launch refreshes only existing
user skill entrypoints (including known legacy/shared copies), with backups.
Missing or removed skills stay absent. Options → Skills shows **Up to date**
when files match the running app's bundled version; click it to check again.
User-managed symlinks are not overwritten. `agent doctor` checks installed bytes,
not whether a running agent has loaded/enabled the skill. Use the agent's own
skill controls and reload at a convenient session boundary if needed.

`fix` repairs skills, hooks and wrappers for detected agent configurations, so use
it for requested setup/repair, not read-only diagnosis. It can reinstall a removed
skill. Hooks (`hooks setup` / `hooks uninstall`) are separate from skill text;
uninstall without `--skills-only` also removes owned wrappers. Installation does
not rewrite project `AGENTS.md` or `CLAUDE.md`.

## GUI features

These features have no dedicated public CLI command. Guide the user to the UI;
do not invent `flowmux review`, `search`, `editor` or `usage` commands. Shortcuts
below are defaults and can be customized in Options.

| Feature | How to use |
|---|---|
| Code Review | Pane menu/footer button or `Ctrl+Alt+E`; toggles back to the previous tab. Review working changes or recent commits, add code comments and send them to a selected running agent. Scope follows the pane's repository/worktree. |
| Terminal search | `Ctrl+Shift+F` with terminal focus searches that terminal. `Ctrl+Alt+Shift+F` or the magnifier next to Files searches all open terminal tabs, including SSH. Results jump/highlight; Refresh updates results. Only retained scrollback is searchable. |
| Files and editor | Files button; double-click a file to edit. `Ctrl+S` / macOS `Cmd+S` saves. Search with editor focus searches editor content/workspace instead of terminal output. |
| Worktrees | Worktrees sidebar manages repository worktrees; select the intended pane/project first. |
| AI Usage | AI Usage button opens/closes usage information; click again to dismiss. |
| Session history | `Ctrl+Alt+J`; saved sessions are distinct from the live agent list. |
| Workspace overview / pane zoom | `Ctrl+Alt+K` / `Ctrl+Alt+M`. |
| Preferences | Options controls themes, fonts and keybindings. |
