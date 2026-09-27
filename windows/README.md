<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Native Windows host — development build

This is a separate Rust workspace. It opens native Windows workspaces, panes
and tabs, embeds an xterm.js terminal in WebView2, and runs Windows PowerShell
through ConPTY. WSL is not used by the installed application.

This implementation is in progress. It is **not feature-equivalent to the
Linux/macOS application**. See [IMPLEMENTATION.md](IMPLEMENTATION.md) and
[acceptance.json](acceptance.json) for the complete scope and outstanding gates.
Do not mark a release complete based on the initial terminal smoke test.

Build on Windows with Rust MSVC, the Windows SDK, and WebView2 Runtime 109+:

```powershell
cargo build --manifest-path windows/Cargo.toml --locked
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
Use `flowmuxctl.exe --help` to see the commands currently implemented.

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

`read-screen --surface` reads an inactive tab without activating it. Its result
waits for xterm to parse the captured output sequence; physical row breaks are
preserved, including wraps in narrow panes.

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
screens, running programs, agent resumption, session-local prompt customizations, window
geometry and terminal settings are not yet restored. Crash recovery uses the
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
