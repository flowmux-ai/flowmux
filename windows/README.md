<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Native Windows host — development build

This is a separate Rust workspace. It opens native Windows workspaces, panes
and tabs, embeds an xterm.js terminal in WebView2, and runs native Windows shells
through ConPTY. WSL is not used by the installed application.

This implementation is in progress. It is **not feature-equivalent to the
Linux/macOS application**. See [IMPLEMENTATION.md](IMPLEMENTATION.md) and
[acceptance.json](acceptance.json) for the complete scope and outstanding gates.
Do not mark a release complete based on the initial terminal smoke test.

Build on Windows with Rust MSVC, the Windows SDK, and WebView2 Runtime 109+:

```powershell
cargo build --manifest-path windows/Cargo.toml --locked
Copy-Item windows/target/debug/flowmux-command.exe windows/target/debug/flowmux.com
python windows/scripts/fetch-conpty.py --output windows/target/debug
windows/target/debug/flowmux.exe
windows/target/debug/flowmuxctl.exe doctor
windows/target/debug/flowmuxctl.exe --json tree
```

The committed terminal assets allow builds without Node. To change them:

```sh
cd windows/terminal
npm ci
npm test
npm run build
```

To cross-build from Linux with cargo-xwin and Clang/lld installed:

```sh
cargo xwin build --manifest-path windows/Cargo.toml --target x86_64-pc-windows-msvc --locked
cp windows/target/x86_64-pc-windows-msvc/debug/flowmux-command.exe windows/target/x86_64-pc-windows-msvc/debug/flowmux.com
python3 windows/scripts/fetch-conpty.py --output windows/target/x86_64-pc-windows-msvc/debug
cargo test --manifest-path windows/Cargo.toml --locked
cargo xwin clippy --manifest-path windows/Cargo.toml --target x86_64-pc-windows-msvc --all-targets -- -D warnings
```

The Named Pipe is per window. A protected DACL allows the owning user and
SYSTEM, and remote pipe clients are rejected. Terminals receive
`FLOWMUX_PIPE_NAME`; callers outside a terminal can pass `--pipe` explicitly.
The discovery files live under `%LOCALAPPDATA%\flowmux\windows\instances`.
They are published by atomic replacement after the pipe is bound. The CLI checks
the process ID reported by Windows before sending commands. An unavailable
explicit or inherited pipe returns its connection error; it never redirects the
command to another window. Connecting and disconnecting before sending a request
does not stop the listener. See [IPC verification](evidence/2026-09-28/ipc.md)
for coverage and remaining limits.
Each window admits up to 16 pipe clients and 16 outstanding GUI requests. IPC
uses fixed deadlines: 3 seconds to acquire a connection, 5 seconds to send/receive
a request or write a reply, 15 seconds for the GUI command, and 25 seconds for
the CLI to receive its reply. A completed reply waits up to 2 seconds for client
closure. A timeout after dispatch does not prove a command was cancelled; check
the window state before repeating a mutation. Commands are never automatically
retransmitted. See [deadline and shutdown evidence](evidence/2026-09-28/ipc-limits.md).
Use `flowmuxctl.exe --help` to see the commands currently implemented.

`paste` supplies explicit text to a terminal; it never reads the system clipboard.
Unlike `send-keys`, it uses xterm's paste operation after previously received PTY
output has been parsed. LF and CRLF become CR, and the current bracketed-paste
mode controls the surrounding `ESC[200~` / `ESC[201~` sequences. Unicode codepoints
are preserved without NFC/NFD normalization. Literal control sequences in the
text are preserved too; bracketed paste is not a content sanitizer.

```powershell
flowmuxctl.exe paste --surface surface:<id> -- "한글 입력"
flowmuxctl.exe paste --pane pane:<id> -- "first`nsecond"
```

Omitting the target follows the calling surface, or the active tab for an
external caller. An explicit inactive surface retains focus and its own mode.
The UTF-8 text limit is 128 KiB, excluding automatically added brackets; NUL,
invalid Unicode, unavailable input and composition in progress are rejected.
Empty text is a no-op. The CLI reports `accepted_bytes`, `bracketed`, `sequence`
and `delivery: queued` after the native input queue accepts the complete payload.
That receipt does not prove a shell consumed or executed it. A terminal permits
one pending CLI paste, with a 12-second parser deadline; expired replies never
inject input. As with other mutations, a lost IPC reply after queueing must not
be automatically retried. Windows command-line limits still apply to CLI text.

Browser paste events within the terminal use the same bounded xterm path, once
per event, using its currently parsed mode. They do not pass through the
Shift+Enter key mapping. Rejection appears inside the terminal without moving
focus. The Find field retains its own text editing. Paste during IME composition
and xterm's deferred finalization is rejected without committing, cancelling or
replaying the composition.
Desktop clipboard/menu/shortcut behavior and real IME interaction remain pending;
the hidden verifier uses explicit test text and never accesses the OS clipboard.
See [paste evidence](evidence/2026-09-28/paste.md).

Terminal **Ctrl+Shift+C** (also **Ctrl+Insert**) copies the selected text;
**Ctrl+Shift+V** and **Shift+Insert** paste through the bounded paste path.
Ctrl+C retains its terminal interrupt behavior. Right-click opens **Copy**,
**Paste**, **Select all** and **Clear selection**. When an application enables
terminal mouse reporting, hold Shift with the right-click to open this menu.
Arrow keys, Home/End and Escape navigate/close the menu. Search fields retain
their own text editing. These UI paths are implemented; actual desktop shortcut,
mouse, accessibility and IME acceptance is still pending.

A selection snapshot keeps the original text available when a TUI redraws or
clears the selected cells. A new primary selection, explicit clear, new search,
normal text/key input, paste, buffer switch or terminal reset discards it.
Tab hiding/moves and process exit preserve the surface's snapshot. Copying no
selection leaves the clipboard alone. A selection over 128 KiB of UTF-8 is
rejected without truncation or fallback to older text. This bounds retained
snapshots; extracting a very large selection can still temporarily allocate a
larger xterm string. Clipboard writes do not normalize Unicode. Browser/platform
newline conventions still apply.

Clipboard APIs are called only from the terminal's user actions. A clipboard
read that completes after focus, tab visibility, composition or another input
changed is discarded. Requests are not retried, and a pending request prevents
additional concurrent requests. Permission/API failures appear inside the
terminal. Hidden debug hosts disable these clipboard actions and the automatic
WebView2 clipboard-read permission grant. The verifier never reads, replaces or
restores the user's clipboard.

Selection automation is independent of the OS clipboard:

```powershell
flowmuxctl.exe selection --surface surface:<id> read
flowmuxctl.exe selection --surface surface:<id> all
flowmuxctl.exe selection --surface surface:<id> range 5 0 12
flowmuxctl.exe selection --surface surface:<id> clear
```

`range` takes zero-based buffer row, column and cell length; rows include
scrollback in the current normal/alternate buffer. Requests wait for previously
received output to be parsed and do not activate the target tab. Results include
the exact text, `live`/`retained`/`none` source, buffer and selection coordinates.
Retained coordinates describe the original selection and may no longer point
to the same text. `result.error` reports an invalid range or oversized selection;
transport/target errors use the normal CLI error response. See
[selection evidence](evidence/2026-09-28/selection.md).

The side panel's Notifications button opens a native list with unread counts on
workspace and tab captions. Entries can reopen their original terminal after a
move, including its retained screen after process exit. Opening a closed source
returns an error; it does not select a replacement tab or mark the entry read.
The list is per window, memory-only, oldest first, and retains at most 50 entries.

```powershell
flowmux notify --surface surface:<id> --title "작업 완료" --level attention "확인이 필요합니다"
flowmux notify-complete --agent "My agent" --message "task complete"
flowmux notifications list --unread
flowmux notifications show
flowmux notifications open <notification-id>
flowmux notifications jump-to-unread
flowmux notifications mark-read <notification-id>
flowmux notifications delete <notification-id>
flowmux notifications clear
```

`notify` targets the explicit surface/pane, the invoking terminal, or the active
terminal, in that order. `--global` creates a source-free entry. Title/body limits
are 1/8 KiB of UTF-8; NUL and completely empty notices are rejected. Supported
input levels are `info`, `attention`, `error`, `completed`; list output uses the
shared domain names `info`, `needs_input`, `error`, `turn_completed`. Near-duplicate
notices from the same pane and surface are suppressed for eight seconds unless
attention priority increases, even if their bodies differ. Global entries are
not deduplicated. Moving between panes changes the dedup key; source navigation
still uses the stable surface ID. Focused info/completed notices are suppressed;
attention/error notices can still be retained. List is read-only; show marks
existing entries read. Opening a source acknowledges its retained notices.

Received ConPTY output is also inspected for OSC 9/99/777, using the existing
Rust parser. Numeric OSC 9 cwd/progress subcommands are excluded. The streaming
extractor bounds unfinished payloads and discards invalid UTF-8; the parser
sanitizes controls and outer whitespace without Unicode normalization. At most
16 notices are considered per output chunk; later higher-priority notices can
replace earlier lower-priority entries in that bounded batch. Restored history is not replayed
into the notification store. This supports the existing simple notification
formats, not all Kitty OSC 99 multipart features. `tree` exposes received output
byte counts and the last observed timestamp; these do not determine agent state.

Windows desktop toast delivery, toast activation and OS taskbar badges are not
implemented yet. CLI responses report `desktop_delivery: "not_implemented"`.
Actual foreground/IME/menu behavior remains separate from the hidden native
verification. See [notification evidence](evidence/2026-09-28/notifications.md).

The installer includes three entry points. **flowmux.exe** is the GUI target
for desktop shortcuts. **flowmux.com** is a console executable: with a command
it uses the same IPC client as **flowmuxctl.exe**, and without a command it
starts the sibling GUI and returns. With the normal Windows PATHEXT ordering,
typing `flowmux` in CMD or PowerShell selects `flowmux.com`. The installer does
not modify PATHEXT. If your shell changes that ordering, call `flowmux.com` or
`flowmuxctl.exe` explicitly for scripts and pipelines. An explicitly invoked
GUI `.exe` remains subject to the shell's GUI-process waiting behavior.

```powershell
flowmux --help
flowmux --version
flowmux --json tree
flowmux new-tab --shell cmd
flowmux --shell=cmd --cwd='C:\Projects'
flowmux --json --new-window --shell powershell
```

GUI and console launchers use one grammar: launch options accept both separated
and `=` values, and cannot be combined with a CLI command. The unified help lists
both launch options and commands. The console launcher's `--json` response is
`{"spawned_pid":1234}`: it confirms process creation, not WebView/PTY readiness.
The new host handles its own startup errors. `flowmuxctl.exe` always requires a
command and never opens a GUI. No entry point replays an IPC command.

CLI success/help/version exit with 0, argument errors with 2, and runtime/output
errors with 1. Successful responses use stdout; diagnostics use stderr. With
`--json`, runtime errors are JSON objects containing `error`; argument errors
remain clap's textual usage diagnostics. `read-screen` without `--json` emits
plain text. Pipes and redirected files use UTF-8. The GUI's CLI mode preserves
redirected streams when attaching to its parent's console. Closed output pipes
return an error instead of panicking; a command may already have taken effect.
CLI errors do not open message boxes. See
[entry-point verification](evidence/2026-09-28/entrypoints.md).

When launched from a native flowmux terminal, both launchers ask that terminal's
GUI host to create the new window. The new GUI therefore survives closing the
invoking tab, a clean source-window exit, or termination of the source host.
Ordinary terminal descendants remain in their original kill-on-close job.
The broker launches its own `flowmux.exe` build. It carries the caller's native
UTF-16 arguments, current directory and environment, including updated PATH;
relative `--cwd` is resolved from the caller. The previous terminal's flowmux
routing variables are removed, and new PTYs receive their own identifiers.
An obsolete inherited pipe or surface returns an error without another target
or a local launch fallback. A timeout may still mean the host created a window;
never repeat a launch automatically. The child still inherits any jobs containing
its GUI broker; broader external-job policies have not been validated. See
[independent-window verification](evidence/2026-09-28/window-lifetime.md).

Windows PowerShell remains the initial default. Use **Settings…** to choose
Windows PowerShell, Command Prompt, or installed PowerShell 7 for future tabs
and workspaces. Right-click **+ Tab**, or choose **New tab with shell…** in the
settings menu, to select a built-in profile for one tab. Splits inherit their
source terminal's shell and arguments unless explicitly overridden. Existing
processes keep their shell when the default changes.

```powershell
flowmux.exe --new-window --shell cmd
flowmuxctl.exe shells
flowmuxctl.exe settings shell cmd
flowmuxctl.exe settings shell powershell --arg=-NoProfile
flowmuxctl.exe new-tab --shell powershell --shell-arg=-NoProfile --cwd 'C:\Projects'
flowmuxctl.exe split horizontal --shell cmd
flowmuxctl.exe new-workspace --shell cmd --cwd 'C:\Projects'
```

`--shell` also accepts an absolute `.exe`/`.com` path or a bare executable name
on the host's PATH. Each repeated `--shell-arg` is one argument, with no command
string splitting. `--shell-arg=` supplies an empty argument even through legacy
shell argument marshalling. The settings equivalent is `settings shell PROGRAM
--arg=VALUE`; program and argv persist together. Built-in `powershell` and `cmd`
resolve under System32; `pwsh` searches PATH and then `ProgramFiles\PowerShell\7`.
Bare custom names search absolute PATH entries, in order, using the executable
`.COM`/`.EXE` entries from PATHEXT. Relative executable paths and implicit batch
files are rejected. Select their interpreter explicitly when needed.

Built-in PowerShell profiles add the session-local prompt integration. The CMD
profile uses `/D` (no registry AutoRun) and prefixes only the child's `PROMPT`
with a cwd report; it leaves console encoding and the rest of the prompt intact.
An explicit executable path/name such as `powershell.exe` is a raw executable:
it receives only the supplied arguments and no automatic prompt integration.
Custom interpreters, including `cmd /C`, have their own argument parsing rules;
argv escaping is based on the Microsoft C runtime convention. PowerShell 5's
native argument marshalling also requires care with embedded quotes.

Check `tree` for each terminal's `shell`, `ready`, `running` and `startup_error`.
A new-tab reply creates the terminal; startup completes asynchronously. New-tab
requests reject invalid paths/cwd before changing layout. A later CreateProcess failure
leaves a saveable terminal with an error and **Start Command Prompt** recovery
button. To retry it through the CLI:

```powershell
flowmuxctl.exe retry-shell --surface surface:<id> --shell cmd
```

Retry preserves the surface and history and never replaces a running/exited
process. Saved terminals restart their recorded program and arguments in fresh
processes, so explicit startup commands run again. The display history itself
is never executed. Older state without shell records keeps Windows PowerShell,
even if the current default is CMD. If a restored executable has disappeared,
other terminals still start and the failed tab offers explicit recovery.
Explicit relative `--cwd` paths are resolved where the CLI/launcher is invoked.
Omitting `--cwd` inherits the source terminal's recorded directory. Raw IPC
relative cwd values use the source terminal; ambiguous drive-relative values
must be sent as absolute paths.
PowerShell 7, other interactive shells, script-language quoting, real menus/IME,
DPI and accessibility acceptance remain pending. See
[shell verification](evidence/2026-09-28/shells.md).

The side panel's **Workspace…** menu (also available by right-clicking a workspace)
renames, colors, moves up/down and closes workspaces. Right-click a tab to rename
it. Names preserve Korean, decomposed Unicode, emoji, spaces and literal `&`;
empty names, control characters and names over 256 UTF-16 units are rejected.
A custom tab name stays locked when the shell changes its automatic title.
The native name/color editor uses **Apply** and **Cancel** buttons. Enter/Escape
while editing text stay with the native edit control; Tab can reach the buttons.

```powershell
flowmuxctl.exe workspace list
flowmuxctl.exe workspace current
flowmuxctl.exe workspace focus workspace:<id>
flowmuxctl.exe workspace rename workspace:<id> "한글 프로젝트"
flowmuxctl.exe workspace color workspace:<id> "#75a3ff"
flowmuxctl.exe workspace color workspace:<id> --clear
flowmuxctl.exe workspace reorder workspace:<id> 0
flowmuxctl.exe rename-tab surface:<id> "테스트 셸"
flowmuxctl.exe workspace close workspace:<id>
```

Reordering uses a zero-based position and preserves active identity, terminal
processes and pane geometry. `workspace current` follows the calling surface's
workspace, even while hidden or after a tab move. Metadata changes preserve
terminal focus. Names, colors, order and locked tab titles persist on restart;
older state without a color remains readable.

Closing a workspace terminates all its terminals and descendant processes.
The native menu asks before closing; an explicit CLI close performs the action.
Closing the active workspace selects its next neighbor, or its previous one at
the end. The final workspace is currently protected; use `quit` to close the
window. Empty-window UI, drag reordering, overflowing side panels, automatic-name
reset and native menu/dialog/IME/DPI acceptance remain pending. Hidden verification
is available in `scripts/verify-workspaces.ps1`.

The side panel's **Settings…** menu changes terminal font/fallback list, size
(6–72 pixels), dark/light theme, cursor blink/style and scrollback (0–100000
lines), plus minimap enable/width/opacity. Larger/smaller/reset text commands change the shared font size. Lowering
scrollback discards the oldest retained lines. Terminal settings are shared
across this user's Windows flowmux windows and apply to existing and new tabs
without restarting their shells or requesting focus. An active terminal IME
composition or history restore defers changes until it ends.

```powershell
flowmuxctl.exe settings show
flowmuxctl.exe settings set font-family 'Cascadia Mono, Consolas, monospace'
flowmuxctl.exe settings set font-size 18
flowmuxctl.exe settings set theme light
flowmuxctl.exe settings set cursor-blink false
flowmuxctl.exe settings set cursor-style bar
flowmuxctl.exe settings set scrollback 20000
flowmuxctl.exe settings set minimap-enabled true
flowmuxctl.exe settings set minimap-width 40
flowmuxctl.exe settings set minimap-opacity 50
flowmuxctl.exe settings reset
```

Settings, including the default shell/argv, use versioned UTF-8 JSON at
`%LOCALAPPDATA%\flowmux\windows\config.json`. Writes acquire an exclusive lock,
merge the changed field with the latest file and atomically replace it. A failed
save preserves the previous file and current options. Native edits detect a
competing change to the same value; the CLI also supports `--expected <value>`.
Other windows and external edits are observed about once per idle second.
`settings show` reports the desired document, `config_error` and each terminal's
applied acknowledgement; a successful set means the file was saved, while an
IME composition can still defer an individual terminal's application.

Invalid or future-version files are preserved and reported by **Settings (!)…**.
An already running window retains its last valid options; a newly opened window
uses defaults. Only an explicit reset replaces an invalid file. Reset also
restores the default shell to Windows PowerShell. Temporary
windows share settings too; `--temporary` disables window-state persistence.
Hidden debug hosts use volatile defaults unless the verifier explicitly supplies
an isolated `FLOWMUX_TEST_CONFIG_DIR`.

Use installed monospaced fonts; Unicode font names are preserved, but font
availability and glyph coverage are not validated. Settings currently affect
terminal colors, not native controls or the find bar. Custom themes, font
discovery, configurable zoom shortcuts, per-tab settings, real menu/IME/DPI and
accessibility acceptance remain pending. See
[settings verification](evidence/2026-09-28/settings.md).

The terminal minimap is enabled by default at 40 CSS pixels wide and 50% opacity.
Width accepts 12–96 and opacity 0–100. It previews a movable window of retained
normal-buffer cells, with one physical row per CSS pixel. The mouse wheel over
the minimap moves **only the preview** by 25 rows per event; click/drag centers
the actual terminal viewport at the pointed row. Keyboard focus on the minimap
supports arrows, Page Up/Down, Home and End. Composition blocks navigation.

The minimap reserves a separate gutter. Alternate-screen entry hides its raster
while keeping that gutter, so a TUI does not gain/lose columns on every mode
switch. Explicit enable/width changes can resize/reflow the terminal. Native
menu/CLI settings share persistence and live propagation; composition defers
these changes with the other terminal settings. Disabling resets the preview.

Rendering reads xterm cell widths and foreground/background colors, including
wide Korean cells, truecolor, ANSI palette, inverse, dim and invisible attributes.
Updates keep a 100 ms deadline during output; inactive tabs release their raster
and skip painting while their parsers continue. The preview is bounded to 2048
rows and canvas scaling to 2 device pixels per CSS pixel. Very tall windows show
only that bounded preview. Large-pane/multi-pane sustained load, physical
pointer/keyboard/IME/accessibility and high-DPI visual acceptance remain pending.
Applications' OSC palette changes are not yet reflected by the minimap.

```powershell
flowmuxctl.exe minimap --surface surface:<id> read
flowmuxctl.exe minimap --surface surface:<id> preview -25
flowmuxctl.exe minimap --surface surface:<id> seek 120
```

These operations wait for preceding output parsing and do not request keyboard
focus. `read` reports current geometry, preview bounds and a checksum of the
actual canvas pixels; it refreshes a dirty visible preview before returning.
`preview` takes signed rows (negative means older), and `seek` centers on a
zero-based physical buffer row. Rows are not stable references after reflow or
history eviction. Hidden/disabled/alternate/composing views reject navigation;
reads remain available and report visibility. Terminal selection is preserved.
See [minimap verification](evidence/2026-09-28/minimap.md).

Move an existing terminal between panes or workspaces in the same window:

```powershell
flowmuxctl.exe move-tab surface:<id> --to-pane pane:<id> --index 0
flowmuxctl.exe read-screen --surface surface:<id>
```

The native **Move tab…** menu exposes destinations and left/right reordering.
Moving retains the process and WebView. An empty source pane/workspace collapses.
Inside a terminal, commands with an omitted target resolve its stable surface ID,
including after a move or while hidden. `identify` returns its current location;
the shell's inherited pane/workspace environment variables still describe spawn
time and cannot be rewritten in a running child process.

Drag a divider to resize nested panes. **Alt+Arrow** focuses the nearest pane in
that direction; **Ctrl+Alt+M** or **Maximize pane / Restore pane** toggles the
focused pane's size. Shortcuts defer to active IME composition and AltGr input.
Maximizing hides siblings while their terminals continue processing output.
Restoring reuses the original split ratios. Focusing a different pane, changing
workspace, splitting, closing, moving a tab or resizing that workspace restores
the split layout. A one-pane workspace is already full size.

```powershell
flowmuxctl.exe resize-pane pane:<id> --ratio 0.6
flowmuxctl.exe focus-direction left --pane pane:<id>
flowmuxctl.exe toggle-pane-zoom pane:<id>
```

`resize-pane` accepts a split ID or a leaf pane's immediate parent. The ratio is
the **first** child's width/height, including when targeting the second child.
Finite ratios strictly between 0 and 1 are accepted and clamped to 0.05–0.95.
Resizing an inactive workspace preserves the active workspace and focus.
Omitted navigation/zoom targets use the calling surface's current pane.
Directional navigation with no neighbor makes no change. Ratios persist across
restart; maximization is temporary and restarts with the ordinary split layout.
`tree` reports `zoomed_pane`, pane/divider rectangles and each WebView's bounds and
controller visibility (a hidden test host's parent window still stays hidden).
Native drag, keyboard/IME transitions and DPI acceptance remain pending;
`scripts/verify-panes.ps1` tests the shared behavior through a hidden native host.

`read-screen --surface` reads an inactive tab without activating it.
`capture-pane` is an alias with the same arguments; it does not implement tmux's
capture flags. Both wait for xterm to parse the captured output sequence and
read the **current scrolled viewport**, preserving physical row breaks including
soft wraps. Trailing row spaces are trimmed; blank rows are kept. Unicode is
not normalized. Reads do not change selection, scrolling or focus.

```powershell
flowmuxctl.exe capture-pane --surface surface:<id>
flowmuxctl.exe read-screen --surface surface:<id> --recent --json
```

`--recent` instead reads the latest 80 physical normal-buffer rows (including
blank bottom rows), regardless of scrolling or cursor position. In the alternate
buffer it reads the whole screen, including status/footer rows below the cursor.
This provides bounded input for future agent status detection; it does not yet
classify agents or automatically poll their screens. No full history is copied
on each output event. Reads still work after process exit until the tab is closed.

JSON keeps `surface`, `sequence` and `text`, and adds `screen` metadata: mode,
normal/alternate buffer, dimensions, buffer length, first row, row count, viewport
and base rows, and cursor position. Rows/columns are zero-based cells in the
current buffer; they are not stable references after eviction or reflow.
`sequence` is the completed parser sequence at extraction, at least the request's
barrier. Later output may already have been parsed; it is not a historical replay
at the exact barrier or a synchronized snapshot of several terminals.

A text response is limited to 128 KiB UTF-8 including newlines. Oversized reads
fail with a CLI error rather than returning truncated text or waiting for the
bridge deadline. Extraction checks each row and stops on the first exceeding the
limit. The bound does not constrain a single row's temporary string allocation.
Plain CLI output remains text with a final newline; JSON exposes metadata.

PowerShell reports its current local drive directory after each prompt. New
tabs, splits and workspaces inherit that directory, including after a tab move.
A child `flowmuxctl` also supplies its working directory, so `cd` followed by
`flowmuxctl new-tab` on the same command line works before the next prompt.
`identify` and `tree` expose the recorded directory; `tree` also reports whether
the surface has supplied a live directory.

The startup integration is session-local: it wraps the existing prompt without
editing profiles, execution policy, PSReadLine functions/key bindings or console
encoding. ASCII percent-encoded OSC 7 paths preserve Korean, decomposed Jamo and
combining marks even with code page 949. Local OSC 9;9 reports are also accepted.
If a prompt tool replaces the wrapper later, reinstall it in that shell:

```powershell
Invoke-Expression (& $env:FLOWMUX_BUNDLED_CLI_PATH shell-integration | Out-String)
```

Reported paths must be absolute local drive paths. UNC, device/verbatim paths,
foreign file-URI hosts and non-filesystem providers are not tracked; the last
local directory is retained. Reports are metadata and are not opened or probed.
Long-path process startup, other shells and arbitrary prompt frameworks remain
unverified. Constrained Language mode skips the automatic prompt wrapper.
See Microsoft's [current-directory integration guidance](https://learn.microsoft.com/en-us/windows/terminal/tutorials/new-tab-same-directory).

The native **Find** button and **Ctrl+Shift+F** open a terminal's find bar.
It supports match case, regular expressions, next/previous with wrap,
**Enter** / **Shift+Enter**, and **Escape** to close while the query is focused.
IME composition owns its Enter/Escape events. Invalid expressions clear the old
selection and display an error; an empty query clears the search. CLI callers
use the same controller after pending output has been parsed:

```powershell
flowmuxctl.exe find "한글" --match-case
flowmuxctl.exe find "error|warning" --regex --previous --surface surface:<id>
flowmuxctl.exe find --close --surface surface:<id>
```

CLI find does not take keyboard focus or activate an inactive tab. It reports
the matched selection, cell position, active buffer and parser sequence; it
does change the target terminal's selection and scroll position. Search covers
retained normal history or the active alternate screen, including exited tabs.
Output and resize invalidate cached search lines before the next query, including
in-place rewrites that leave the cursor unchanged. Queries are single-line and
bounded to 1024 UTF-16 units; returned selection text is bounded to 16384 units
without splitting surrogate pairs. Regex uses the pinned xterm addon's JavaScript
semantics, which differ from VTE/PCRE2. Arbitrary expression performance and
native keyboard/IME/DPI verification remain separate work.

**Search all** opens a native Windows search window for retained output across
all workspaces in this window. It uses literal matching with an optional match
case checkbox. Results show one match per logical line, joining soft wraps,
with 500 rows per page. Double-click or **Open result** selects the matching cells
in the owning terminal. **Refresh**, **Cancel search**, **Previous 500** and
**Next 500** operate on the same service as the CLI:

```powershell
$search = flowmuxctl.exe --json search-all "한글" | ConvertFrom-Json
flowmuxctl.exe --json search-results $search.search
# After pending becomes false, open the first result on this page:
flowmuxctl.exe search-open $search.search 0
flowmuxctl.exe search-cancel $search.search
```

`search-all "한글" --offset 500` starts the next page. Each search replaces the
previous search in this window and invalidates its references. Polling reports
progress and terminals that changed, closed or timed out; unavailable terminals
are not silently counted as having no matches. Pages rescan current output, so
counts can change while shells continue writing. Scanning yields between row
batches and never focuses hidden tabs. Normal history and the active alternate
screen are searched separately; this does not search other application windows.

Result activation verifies the retained text before changing workspace/focus,
then checks again after activation in case the view resized. Normal-buffer
markers allow later appended output and tab moves while rejecting rewritten,
evicted or reflowed rows. A failed check asks for a fresh search. Alternate-screen
results expire when that screen changes. Query limits match single-terminal
find; result previews and labels are bounded. SSH integration, sustained-output
search/load testing and native mouse/keyboard/IME/DPI acceptance remain pending.

Window layout and styled normal-buffer history are saved every 30 seconds and
before a normal close under `%LOCALAPPDATA%\flowmux\windows\state`. Each window
holds an exclusive OS file lease; another process cannot restore or overwrite
its state while it is open. A normal launch restores the latest available closed
window. These options control startup and checkpoints:

```powershell
flowmux.exe --new-window
flowmux.exe --cwd C:\projects
flowmux.exe --restore-window <window-uuid>
flowmux.exe --temporary
flowmuxctl.exe save-state
flowmuxctl.exe quit
flowmuxctl.exe quit --discard-state
```

`--cwd` starts a fresh persistent window. `--temporary` neither reads nor saves
window state. `tree` reports the window UUID, state path, save activity and last
save error. Failed saves retain the previous file and leave the window open;
`quit --discard-state` explicitly closes without taking a new checkpoint.
The native close dialog offers that choice on a save error. Corrupt or unknown
state files are preserved; automatic startup skips them, while an explicit
restore reports the error.

Restoration retains workspace/pane/tab identities, order, focus, titles and
the latest reported local directories, and creates fresh PowerShell processes. History
is rendered before spawning the new process; historical commands and terminal
replies never become shell input. The old display is moved into scrollback so
ConPTY's initial clear does not erase it. Scroll up to read or search it.
Each history is bounded to 128 KiB of complete serialized rows (oldest rows are
removed), with a 32 MiB file limit and at most 128 saved terminals. Alternate
screens, running programs, agent resumption, session-local prompt customizations and window
geometry are not yet restored. Terminal settings reload from the shared config.
Crash recovery uses the
last completed checkpoint; later output can be lost.

The app loads the pinned Microsoft ConPTY DLL from its own installation
directory, with the SDK's `x64/OpenConsole.exe` layout. It does not fall back
silently to an older system implementation. A native stress test reproduced
one leaked process handle per closed session in the validation machine's
inbox ConPTY. See `conpty.lock.json` for the package hash and version.

Native smoke, lifecycle and tab-move checks default to a hidden debug host. They
do not display a window or take keyboard focus. A release build is rejected before
launch because the background test switch is deliberately absent from it:

```powershell
powershell -NoProfile -File windows/scripts/verify-native.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-lifecycle.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-tab-move.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-output-load.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-state.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-cwd.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-find.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-output-search.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-settings.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-shells.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-entrypoints.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-window-lifetime.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-paste.ps1 -BuildDirectory windows/target/debug
powershell -NoProfile -File windows/scripts/verify-selection.ps1 -BuildDirectory windows/target/debug
```

Ordinary test hosts use `--temporary`; state and cwd verifiers use unique directories
via the debug-only `FLOWMUX_TEST_STATE_DIR`, including for crash/restart tests.
Hidden debug hosts without an explicit test state directory also disable
persistence. These tests cannot restore or overwrite a user's saved window.

Adding `-Interactive` opts the smoke/lifecycle/tab-move scripts into visible-window verification. The output-load, state, cwd, find and output-search scripts always use hidden hosts. Real
IME tests additionally require an idle desktop and explicit `-Interactive`:

```powershell
powershell -NoProfile -File windows/scripts/verify-korean-ime.ps1 -BuildDirectory windows/target/debug -Interactive
```

The IME script requires an unlocked interactive Windows desktop and the
Microsoft Korean IME. Every keyboard batch verifies its exact window is in the
foreground. It records preedit screenshots and Unicode console input encoded
as UTF-8, separately from the bytes sent to ConPTY. The debug-only
`FLOWMUX_TEST_INPUT_TRACE` environment variable records input bytes **only when
explicitly set** by this test. It can include sensitive input; never enable it
for normal sessions. Release builds omit that trace code.

The lifecycle check verifies final Korean output and exit status after normal
shell exit, then creates its own descendant processes and checks that tab close
and forced termination of its own host remove the whole process tree. An exited
tab retains its screen for selection, search and `read-screen` until closed.

Create an NSIS installer after building the release binaries:

```powershell
powershell -NoProfile -File windows/scripts/build.ps1 -Installer
```

The installer uses a per-user directory, Start menu shortcuts and a user PATH
entry. Its release payload statically links the CRT, so a separate VC++
Redistributable is not required. A missing WebView2 Runtime is obtained through Microsoft's documented
Evergreen bootstrapper, whose Authenticode signature is checked before
execution. An internet connection is required for that dependency installation.
Uninstall preserves user data and removes only the installed payloads.
New-user, offline, upgrade, uninstall and signing checks are tracked separately;
the presence of an installer script does not prove those gates passed.

Reference: [Microsoft WebView2 distribution documentation](https://learn.microsoft.com/microsoft-edge/webview2/concepts/distribution).
