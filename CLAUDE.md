<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Developing flowmux

Rust workspace, edition 2021, declared MSRV 1.93. Use the exact toolchain in
`rust-toolchain.toml`. See [setup](docs/setup.md) for native dependencies and
[contributing](.github/CONTRIBUTING.md) for license checks.

## Build and verify

Run from the repository root:

```sh
cargo check                         # default headless workspace members
cargo check --workspace             # includes GTK applications
cargo run -p flowmux                 # debug GUI
cargo build --release --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/test-ci.sh linux       # on Linux: shared CI gate, coverage + GUI
bash scripts/test-ci.sh macos       # on macOS: shared CI gate, native main-thread GUI
```

The shared gates isolate runtime state and retain failure logs. macOS GTK
tests requiring AppKit must use the native main-thread harness; a generic
`cargo test --workspace` is not a substitute. See [CI reliability](docs/ci-reliability.md).

The binaries are `flowmux` (GUI and CLI delegation), `flowmuxctl` (IPC client),
`flowmux-md-viewer` (Markdown reader), and `flowmux-daemon` (headless handler).
The GUI embeds its IPC server; normal desktop use needs no separate daemon.
Runtime/UI fixes require the live verification described in [AGENTS.md](AGENTS.md).
Preserve active user sessions; reproduce in an isolated instance with separate
state and a short runtime path under `/tmp`.

The committed Monaco bundle supports Rust builds without Node.js. Frontend
changes use `scripts/build-editor-assets.sh`; commit the rebuilt assets with
the source changes. Preserve upstream license markers in generated bundles.

## Implementation map

| Area | Source |
|---|---|
| Workspace, pane, tab, and SSH domain types | `crates/flowmux-core/` |
| Configuration, themes, keybindings, XDG paths, diagnostics | `crates/flowmux-config/` |
| Persistent window state, instance locks, agent sessions | `crates/flowmux-state/` |
| Shared state transitions and headless request handling | `crates/flowmux-daemon/` |
| Unix socket protocol and tmux compatibility parsing | `crates/flowmux-ipc/` |
| CLI commands, hooks, installers, PTY proxy | `crates/flowmux-cli/` |
| PTY spawning, input modes, terminal environment | `crates/flowmux-terminal/` |
| Browser operations, snapshots, selector references | `crates/flowmux-browser/` |
| Editor document I/O, recovery, search, asset server | `crates/flowmux-editor/` |
| OSC parsing and desktop notification transport | `crates/flowmux-notify/` |
| Process inspection and Git/worktree operations | `crates/flowmux-procmon/`, `crates/flowmux-vcs/` |
| Host cookie extraction and profile detection | `crates/flowmux-cookies/` |
| GTK UI and IPC-to-UI dispatch | `crates/flowmux/` |

### GTK and async code

GTK widgets stay on the main thread. The GUI's tokio IPC handlers send
`GtkCommand` values over `async_channel`. The bridge types live in
`crates/flowmux/src/bridge/`; the dispatch loop and handlers live in
`crates/flowmux/src/ui/window/` and return oneshot replies when requested.

Each GUI process binds a per-PID socket from
`flowmux_config::paths::runtime_socket_for_pid`. PTYs receive that path in
`FLOWMUX_SOCKET_PATH`, keeping requests in the originating window. The stable
fallback path is a pointer for CLI calls made outside a pane. Use the shared
path helpers, including their Flatpak handling.

### Surface backends

`ui::ghostty_pane::GhosttyPane` wraps GTK VTE; the name does not indicate a
Ghostty rendering backend. `ui::pane_terminal::PaneTerminal` aliases it and
shares callback types. VTE handles rendering, font fallback, IME, and buffer
text extraction. PTY and input-mode helpers are headless.

Browser and editor views use WebKitGTK on Linux and WKWebView in the macOS
sources. Neither exposes CDP. Browser engine labels select profiles; they do
not switch to Chrome or Firefox. Host-cookie extraction is separate from
WebView insertion, which is not implemented.

The image viewer loads ThorVG dynamically through `ui/thorvg.rs`; missing
ThorVG affects the viewer, not the application build. The helper script builds
v1.0.6 with C API bindings and loaders. GIF decoding uses Rust's `image` crate.

## Agent integration invariants

- Read [AGENTS.md](AGENTS.md) for the pane/browser command contract, including
  the preference for the in-app browser.
- The [Flowmux CLI skill](docs/agent-skills.md) covers product CLI and GUI
  workflows. Keep it user-facing; do not bundle development skills.
- New pane commands accept explicit IDs and use pane context where supported.
  Distinguish a pane's UUID from its individual tab surface UUID.
- Snapshot references belong to the latest snapshot of one browser surface.
  The snapshot must not add tracking attributes to the DOM. Reused token
  names can point to different elements after a new snapshot.
- Hooks and user skill text are embedded in the CLI. Keep `doctor`/`fix`
  drift detection consistent with changes to installed payloads.
- Native lifecycle hooks are activity evidence; process inspection establishes
  identity/liveness; terminal text is fallback evidence. A title or screen
  scan must not rename a hook/process-owned agent. Turn completion is separate
  from session teardown or process death.
- Preserve unrelated agent handlers and notification configuration. Never
  modify the user's Codex hook trust decisions.
- Claude permission events lack a tool-use ID. Retain their batch marker until
  `PostToolBatch`; explicit input-tool waits are correlated by `tool_use_id`.
  Treat repeated starts/resolutions as idempotent. Retain resolved tool IDs
  until the turn/session boundary so delayed starts cannot reopen them.
  Session-level quota/API/input waits remain separate.
- Codex permission events lack a per-call resolution. Track their turn scope;
  ordinary `PostToolUse` must not clear an unrelated parallel permission wait.
  Match parent Stop against observed child identities; parent and child
  `turn_id` values differ. Keep this ledger runtime-only and clear it on
  session/process/surface end.
- Child-start events can be omitted when a child is reused, and another hook
  can block Stop. The observed ledger is not a complete process inventory;
  retain process/screen fallbacks.

The [agent status verification contract](docs/agent-status-verification.md) maps
these rules to regression cases and diagnostic evidence. Keep a failing case's
workspace visibility and event ordering intact; check both early completion and
stuck working before committing lifecycle changes.

## Conventions

Use **side panel**, **workspace**, **pane**, **tab**, **browser tab**, and
**editor tab** consistently in user-facing text. A workspace owns the pane
tree; a pane contains tab surfaces. Use **workspace name** and **tab name**
for their displayed labels. Keep existing code identifiers unless a rename
is explicitly part of the task.

flowmux is GPL-3.0-or-later. Preserve SPDX headers, license texts, attribution,
and vendored notices. New source files need an SPDX header. Imported code or
assets must have compatible licenses and attribution.
