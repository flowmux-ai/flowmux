<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Windows screen composition changes

This stage changes screen roles and layout, following the Linux source and the
repository's Options/notification/Files reference recordings. It does not claim
complete Linux visual or functional parity. Only `windows/` was edited.

- Options replaces the long settings menu and single-value windows with one
  owned, nonmodal 760×720 General/Theme window using existing asynchronous settings
  writes, current values, Apply, error retention, Reload, Reset and Close.
- Notifications replaces the 850×560 list/detail window with a bell-anchored
  320-DIP owned popup, newest-first title/body/time rows, bounded scrolling,
  per-row Open/Delete and All Clear. The presented snapshot precedes read ack.
- Files reserves one dock on the right of the whole workbench, preferred 320 DIP,
  retaining source-pane caches. The tree starts at 58 DIP instead of below a
  permanent 214-DIP action area. Actions opens a conditional destination form;
  its captured source/token survives selection changes. Destinations are relative
  to the Files root. Existing operation validation and editor guards remain.
- Browser navigation occupies one 40-DIP row. Zoom, Downloads and Find move into
  Browser tools. Narrow panes retain the address/tools while progressively hiding
  navigation buttons. Reload/Stop keep identical bounds across loading changes.

The following hidden native cases pass (29 grouped checks, not feature gates):

| Case | Groups | Runner duration |
|---|---:|---:|
| Options entry, pages, native Unicode Apply, terminal ACK, errors, reset/close | 5 | 5.285 s |
| Global Files dock, cached sources, zoom/workspaces, captured native form | 2 | 7.922 s |
| Notification popup geometry, unread snapshot, Unicode, actions/source lifecycle | 9 | 16 s |
| Browser toolbar widths, navigation, popup/tab lifecycle and restart | 8 | 22.945 s |
| Existing Files mutation/byte/editor/stale-token regression | 5 | 17.839 s |

Options/Files/notifications and file-operation regressions used the first freshly
linked debug GUI staged separately from the running app. Browser checks used the
subsequently rebuilt GUI with the geometry correction and clarified Files input
hint; unchanged CLI entrypoints came from the first stage. The final all-target
Clippy run passed in 26.945 seconds. Runtime/source hashes and native observations
are preserved in the [diagnostic manifest](ui-structure-diagnostics/manifest.json).

Both debug Cargo invocations failed at canonical GUI publication (OS error 5)
after linking: 97.796 seconds for all entrypoints, then 113.554 seconds for the
corrected GUI. Fresh deps outputs were checked against each build's time interval
and copied into separate verification directories. The running user's executable
was never stopped, moved or overwritten. These are explicitly failed Cargo
invocations, followed by successful native checks of their new linked artifacts.

Earlier failures remain preserved: missing Win32 imports; an obsolete settings
metadata variant and a Clippy match warning; a Files verifier passing an absolute
destination despite the existing relative-path contract; a notification verifier
rejecting the popup's intended thin border; and a browser fixture missing the
System.Drawing compiler reference. Those causes were fixed before rerunning the
affected scope. Code review also found a real browser regression: metadata refresh
copied stale hidden-button bounds. Both buttons now receive the same current
layout, and native checks cover normal, narrow, compact and restored sizes.
An initial release attempt omitted the required static-CRT flag and was stopped;
its output is not a distributable artifact.

The static-CRT release invocation also failed only at canonical GUI publication
after linking (117.627 s). All three fresh entrypoints were staged and passed
release doctor in 1.035 s. NSIS packaging passed in 12.973 s after setting the
portable NSIS resource directory; the initial attempt failed immediately because
it looked under `/usr/share/nsis`. No installer was executed.

Installer: `windows/dist/flowmux-windows-0.10.1-dev-ui-structure-x64-setup.exe`,
5,878,615 bytes; SHA-256:
`dcf83988afe2a39a3a9cad1976d9f9f0a6b0c740c62d704c61cb0af0da05b547`.
Release doctor is not a release-GUI interaction test. Native UI scenarios used
the staged debug hosts described above. The staging note about leaving existing
executables untouched refers to the running GUI; Cargo did publish its CLI files
at the canonical build paths. Collection initially referenced the wrong release
doctor filename and was corrected to its actual evidence path.

All native test processes used owned hidden HWNDs, isolated config/state and
explicit pipes, with bounded CLI/condition waits and outer process Jobs. No
physical input, foreground changes or OS clipboard was used. The checks observe
real controls and backend state, not a simulated replacement UI. They do not
establish physical menu navigation, outside-click/activation/focus behavior,
real Korean IME composition/candidates, accessibility, high contrast, per-monitor
DPI, or a composed WebView/GPU screenshot. The exercised monitor was 96 DPI.

General/Theme still lacks Linux's complete settings and immediate text-edit
workflow; Keybindings/Update, Agents/usage/sessions, Worktrees and workspace
overview remain missing or incomplete. Terminal search modality, pane action
placement and the OS titlebar also differ. The [screen matrix](../../UI_PARITY.md)
tracks those gaps. All 114 feature rows remain 60 partial / 54 pending.
