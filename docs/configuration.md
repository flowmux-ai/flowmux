<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Configuration reference

The main file is `options.json` in Flowmux's configuration directory.
All fields are optional; omitted values use the built-in defaults.

| Directory | Linux default | macOS default | Environment override |
|---|---|---|---|
| Configuration | `~/.config/flowmux` | `~/Library/Application Support/flowmux` | `$XDG_CONFIG_HOME/flowmux` |
| Data | `~/.local/share/flowmux` | `~/Library/Application Support/flowmux` | `$XDG_DATA_HOME/flowmux` |
| State | `~/.local/state/flowmux` | `~/Library/Application Support/flowmux` | `$XDG_STATE_HOME/flowmux` |

Nonempty overrides take precedence. On macOS, state falls back to the data
directory when `XDG_STATE_HOME` is unset, including an `XDG_DATA_HOME` override.

## options.json

`zoom_percent`, `default_browser_engine`, `focus_border_color`,
`focus_border_opacity`, `focus_border_auto`, `persist_browser_session`, `auto_resume_agent_sessions`,
`process_isolation`, `child_oom_score_adj`, `pane_memory_high`, `pane_memory_max`,
`restore_terminal_scrollback`, `scrollback_lines`, `default_shell`,
`terminal_minimap_enabled`, `terminal_minimap_width`, `terminal_minimap_opacity`,
`system_notifications_enabled`, `agent_bar_mode`, `agent_sort_mode`,
`usage_bar_enabled`, `usage_bar_pins`, `cursor_blink`,
`cursor_blink_interval_ms`, `font_family`, `font_size`,
`editor_minimap_enabled`, `agent_notification_target`, `theme`, `theme_overrides`,
and `keybindings`.
`focus_border_auto` follows theme brightness: light themes use an opaque blue
focus indicator; dark themes use the default pale yellow at 30% opacity.
Turn off **Automatic focus colors** to use the saved color and opacity.
Older settings without this field enable automatic colors only when both
focus settings match the old defaults; custom settings remain manual.
`default_shell` selects the command for new tabs; a per-tab IPC `shell` takes
precedence, then `$SHELL` is used. Invalid commands fall back safely.
`agent_bar_mode` switches Agent Activity from the resizable lower side-panel
area to the compact bottom bar.
`agent_sort_mode` orders the side panel's Agents list: `workspace` (default),
`recently_finished`, or `oldest_finished`. The sort button at the right of the
Agents header cycles through them. `recently_finished` lists working agents
first, earliest request first, then finished agents with the latest finish
first; `oldest_finished` is the exact reverse. The time on each row is the sort
key: when the current turn was requested or the last one ended. flowmux times
these status changes itself, so agents without hooks sort too; the times last
for the running flowmux process, and an agent with no observed finish counts
as the longest finished.

`usage_bar_enabled` defaults to `true`. Use **Pin to usage bar** in **AI Usage**
to choose which Claude/Codex usage windows appear in the footer. The selections
are saved in `usage_bar_pins`: an omitted or `null` provider keeps the automatic
selection; an empty array hides that provider's meters.

The browser uses the platform WebKit backend; Options shows a fixed `WebKit`
label. Legacy `default_browser_engine` values are preserved for existing profile
storage compatibility and do not launch Chrome/Firefox or import their cookies.
`scrollback_lines` defaults to 5,000 and is
clamped to `1,000..=1,000,000` for newly created terminal tabs.

`process_isolation` defaults to `auto`: each terminal tab uses its own systemd
scope on Linux when the user service manager is available. `off` disables it;
macOS does not use systemd. Restart Flowmux after changing this option.
`child_oom_score_adj` defaults to 500 (clamped to `0..=1000`, `0` disables it)
and applies on Linux only when a tab runs without its own scope.
`pane_memory_high` and `pane_memory_max` accept binary sizes such as `64M`,
`4G`, or `64MiB`, and set `MemoryHigh`/`MemoryMax` for new tab scopes on Linux.
Limits are disabled by default; invalid or overflowing sizes are ignored.
A hard limit also disables swap in that scope. These options have no effect
when scope isolation is unavailable.

`terminal_minimap_enabled` defaults to `true`. It reserves a right gutter and
draws terminal cells as pixels without covering terminal text. Scrolling over
the minimap moves its local history window without moving the terminal;
clicking or dragging moves the terminal viewport. It includes the current
screen after commands such as `clear`; setting it to `false` restores the
standard scrollbar. Alternate-screen TUIs show a live preview of the current
screen; minimap navigation is inactive because the TUI owns its history.
`terminal_minimap_width` defaults to `40` pixels and is
clamped to `12..=96`. `terminal_minimap_opacity` defaults to `50` percent and
is clamped to `0..=100`. All three settings apply to open and new tabs.

`zoom_percent` applies to terminal and browser surfaces. Terminal zoom is
rounded to a whole-point font size instead of using VTE's fractional font
scale, avoiding GTK text-damage artifacts during cursor blink. Editors keep
their own zoom per tab and use the last changed editor zoom as the default for
new tabs. The resolved theme plus `font_family` and `font_size` overrides are
applied live to both terminal and editor text; editor selection and cursor
colors follow the same theme.

## Notifications

The side-panel bell opens a larger notification list with agent icons, source
workspace/tab labels, and explicit Information, Completed, Input needed and Error
labels. **All / Unread** tabs combine with separate notification-type and workspace
selectors; their counts follow both selectors. For example, Unread + Errors shows
only unread errors. Filter selections last for the current window. Input requests
are an event category, not a claim that a request is still unresolved.

Opening the list does not mark notifications read. Opening a source tab marks
that source read; other tabs in its workspace remain unread. **Mark all read**
acknowledges every workspace without deleting history. The actions menu separates
**Delete read notifications** from **Delete all notifications**. Read entries keep
normal text contrast and lose their unread dot and bold title.

Clicking an entry follows its tab if it moved to another pane or workspace in the
same window, leaves Code Review, and reveals the terminal's latest output and
input prompt. It does not jump to an earlier conversation position. A closed or
missing source shows an explanation and leaves the notification unread.

The in-memory history retains at most 50 entries, removing read entries first.
It is not restored after restart. Identical same-tab messages within eight seconds
are suppressed; OSC information and hook completion reports are also paired within
that window, keeping the hook's Completed kind and message whichever arrives first.
Distinct input requests and errors are retained.
`system_notifications_enabled` controls OS delivery independently of this list.

## Theme file

The `theme` file in the configuration directory uses Ghostty's `key = value` format. It
supports `font-family`, `font-size`, `background`, `foreground`,
`cursor-color`, `selection-background`, `selection-foreground`, and
`palette = N=#rrggbb` for `N` from 0 to 15. A selected `options.json` theme
preset takes precedence over file colors; font values can still come from
the file. `theme_overrides` apply on top.

flowmux does not automatically read `~/.config/ghostty/config`. Import a
compatible file with `flowmux theme import PATH`, then use **Reload config**
in the command palette. In **Options > Theme**, select **User theme file**
to use imported colors, or **Default** to use the built-in colors. A missing
user file uses the built-in defaults and is labeled in the list. Selecting a
preset does not remove the file; select **User theme file** to return to it.
Custom colors remain in effect when switching sources. The Theme tab shows
how many are active; each **Reset** button restores that field's inherited
color, while **Reset custom colors** restores all five fields.
The live preview shows terminal output, an error, selection and cursor colors,
and a short code sample using the effective colors, including overrides. It
does not execute commands. Each source is labeled **Light** or **Dark**.
Both normal and bright ANSI banks are shown in the preview. GitHub Dark offers
a neutral dark palette; Gruvbox Light uses a warm cream canvas with stronger
accents. The Flowmux Contrast Dark/Light pair prioritizes colored-log legibility.
A `theme = ...` line does not load another theme
file; choose a built-in preset through Options instead. Other unsupported
keys are parsed but do not affect the UI.

## cmux.json

The command palette reads `cmux.json` from the focused local terminal's
current directory. It does not search parent directories. `commands` adds
command-palette entries, and `env` applies to those commands. The parser
accepts `name`, but the UI does not use it. Each command has `id`, `label`,
`run`, optional `cwd`, `target` (`focused_pane`, `split_down`, `split_right`,
or `new_surface`), and `confirm`.
`run` is an argv array, for example `["cargo", "test"]`; use
`["sh", "-lc", "..."]` when shell syntax is needed. JSON line and block
comments are accepted.

## Keybinding overrides

`keybindings` maps action names to arrays of GTK accelerator strings. Omitted
actions keep their defaults; an empty array unbinds an editable action.
Copy and paste are fixed and ignore overrides. For example:

```json
{"keybindings": {"split-right": ["<Ctrl><Alt>r"], "next-workspace": []}}
```

See [keyboard shortcuts](keybindings.md) for action names and context behavior.

## State and environment

`state.json` in the state directory is managed by Flowmux and is not a
user-editable configuration file. Runtime context variables include
`FLOWMUX_PANE_ID`, `FLOWMUX_SURFACE_ID`, `FLOWMUX_WORKSPACE_ID`,
`FLOWMUX_TAB_ID`, `FLOWMUX_SOCKET_PATH`, and optional
`FLOWMUX_BUNDLED_CLI_PATH`. `FLOWMUX_RUNTIME_DIR` can isolate a smoke run;
`FLOWMUX_LOG` sets the tracing filter for console and daily file logging,
for example `debug`. Log paths are described in [setup](setup.md#troubleshooting).
