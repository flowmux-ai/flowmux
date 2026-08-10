---
# SPDX-License-Identifier: GPL-3.0-or-later
name: flowmux-browser
description: Drive the in-app browser pane that ships with flowmux. Use when you need to open URLs, take page snapshots, or interact with web pages from inside a flowmux terminal pane — instead of spawning Playwright / Puppeteer / a system Chromium.
---

# flowmux browser automation

If you are running in a terminal that flowmux spawned (the
`FLOWMUX_PANE_ID` env var is set), prefer the in-app browser for any
read / interact-with-a-page task. The browser pane lives next to the
terminal you are in, so the user can see what you do.

## Detect

```bash
FLOWMUX_CLI="${FLOWMUX_BUNDLED_CLI_PATH:-flowmux}"
[ -n "${FLOWMUX_PANE_ID:-}" ] && echo "inside flowmux"
```

When this is true, use the workflow below. When it is false, the agent
is not inside a flowmux PTY — fall back to whatever the user expects
(curl / Playwright / etc.) unless the user supplied an explicit pane.

## Standard loop

```bash
# Open. If a browser pane already exists to the right, the URL is
# added there as a tab; otherwise flowmux splits the source pane.
PANE=$("$FLOWMUX_CLI" --json browser open https://example.com \
  | jq -r '.browser_pane_opened.pane')

# A new WebView starts at about:blank, so first wait for the requested URL,
# then for that document to finish before taking refs from it.
"$FLOWMUX_CLI" browser wait pane:$PANE --url example.com
"$FLOWMUX_CLI" browser wait pane:$PANE --ready-state complete
# Each wait prints true on success and false on timeout. Do not continue
# to snapshot or act when it prints false.

# Take an interactive snapshot. Returns a Markdown tree with `eN`
# refs, plus a refs map carrying selectors. The DOM stays untouched.
"$FLOWMUX_CLI" browser snapshot pane:$PANE

# Act using ref tokens.
"$FLOWMUX_CLI" browser click pane:$PANE e3
"$FLOWMUX_CLI" browser fill  pane:$PANE e1 "user@example.com"
"$FLOWMUX_CLI" browser type  pane:$PANE "password"      # active element
"$FLOWMUX_CLI" browser press pane:$PANE Enter

# Probe state — stdout is "true" / "false" / integer.
"$FLOWMUX_CLI" browser is-visible pane:$PANE e3
"$FLOWMUX_CLI" browser is-enabled pane:$PANE e3
"$FLOWMUX_CLI" browser is-checked pane:$PANE e7
"$FLOWMUX_CLI" browser count      pane:$PANE ".result-row"

# Read page content.
"$FLOWMUX_CLI" browser text  pane:$PANE e3
"$FLOWMUX_CLI" browser value pane:$PANE e1
"$FLOWMUX_CLI" browser attr  pane:$PANE e3 href
"$FLOWMUX_CLI" browser url   pane:$PANE
"$FLOWMUX_CLI" browser title pane:$PANE
```

## Identifiers

`pane:<uuid>`, `surface:<uuid>`, and bare uuids are interchangeable on
the CLI. Use whichever the previous `--json` response gave you.

`--json` toggles single-line JSON output for easy parsing
(`jq -r .browser_pane_opened.pane`); without it, browser reads and probes
print their raw string, boolean, or integer value.

## Claude Code session messaging

Claude Code 2.1.224+ can message independent Claude sessions on the same
machine. flowmux gives unnamed Claude sessions a stable name and exposes the
live pane-to-session mapping:

```bash
"$FLOWMUX_CLI" --json agents
```

Find the target's `session_name`, confirm it with Claude's `ListAgents` tool,
then send plain text with `SendMessage`. `ListAgents` is authoritative: after
`/rename`, or when Claude was launched outside the flowmux shim, match its
working directory against the `root` in the flowmux output instead.

Use session messaging for an existing Claude, `send-keys` for non-Claude
programs or when messaging is unavailable, and agent teams when a lead must
create teammates. Messages cannot approve permissions, change settings, or
run slash commands; never use another session to bypass a denied permission.

## Ref token lifetime

- Refs are scoped to one snapshot per browser surface. After navigation,
  reload, or a DOM mutation, wait for the expected URL, selector, text, or
  ready state and then take a fresh `browser snapshot`.
- Both `e3` and `@e3` resolve.
- A ref-not-found error means: take a new snapshot first.

## What flowmux does not (yet) do

CDP-only features are intentionally not exposed — viewport/device
emulation, network mocking, full-page tracing, screencast, raw input
injection. WebKitGTK and macOS WKWebView do not expose CDP. If a task strictly
needs them, say so before reaching for Playwright, and keep the
user-visible page in flowmux's pane anyway (mirror URLs / outputs back
in via `flowmux browser open`).

## Anti-patterns

- Do not call `playwright install`, `npx playwright open`,
  `puppeteer.launch`, or system `chromium` / `chrome` to read a
  public URL when `FLOWMUX_PANE_ID` is set.
- Do not modify the page DOM yourself. The snapshot intentionally
  does not stamp `data-flowmux-ref` or any other attribute — the server
  resolves ref tokens to selectors on its side.
- Do not assume a `eN` token from a previous snapshot is still valid
  after the page changed.
