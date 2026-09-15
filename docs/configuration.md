<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Configuration reference

The main file is `$XDG_CONFIG_HOME/flowmux/options.json` (normally
`~/.config/flowmux/options.json`). All fields are optional; omitted values use
the built-in defaults.

## options.json

`zoom_percent`, `default_browser_engine`, `focus_border_color`,
`focus_border_opacity`, `persist_browser_session`, `auto_resume_agent_sessions`,
`restore_terminal_scrollback`, `scrollback_lines`, `default_shell`,
`terminal_minimap_enabled`, `terminal_minimap_width`, `terminal_minimap_opacity`,
`system_notifications_enabled`, `agent_bar_mode`, `usage_bar_enabled`, `cursor_blink`,
`cursor_blink_interval_ms`, `font_family`, `font_size`,
`editor_minimap_enabled`, `agent_notification_target`, `theme`, `theme_overrides`,
and `keybindings`.
`default_shell` selects the command for new tabs; a per-tab IPC `shell` takes
precedence, then `$SHELL` is used. Invalid commands fall back safely.
`agent_bar_mode` switches Agent Activity from the resizable lower side-panel
area to the compact bottom bar.

`default_browser_engine` selects a profile for new browser tabs; all choices
use the platform WebKit backend. Chrome/Firefox labels do not launch those
browsers or import their cookies. `scrollback_lines` defaults to 5,000 and is
clamped to `1,000..=1,000,000` for newly created terminal tabs.

`terminal_minimap_enabled` defaults to `true`. It reserves a right gutter and
draws terminal cells as pixels without covering terminal text. Scrolling over
the minimap moves its local history window without moving the terminal;
clicking or dragging moves the terminal viewport. It includes the current
screen after commands such as `clear`; setting it to `false` restores the
standard scrollbar. Alternate-screen TUIs always hide the minimap and use the
standard scrollbar. `terminal_minimap_width` defaults to `40` pixels and is
clamped to `12..=96`. `terminal_minimap_opacity` defaults to `50` percent and
is clamped to `0..=100`. All three settings apply to open and new tabs.

`zoom_percent` applies to terminal and browser surfaces. Terminal zoom is
rounded to a whole-point font size instead of using VTE's fractional font
scale, avoiding GTK text-damage artifacts during cursor blink. Editors keep
their own zoom per tab and use the last changed editor zoom as the default for
new tabs. The resolved theme plus `font_family` and `font_size` overrides are
applied live to both terminal and editor text; editor selection and cursor
colors follow the same theme.

## Theme file

`$XDG_CONFIG_HOME/flowmux/theme` uses Ghostty's `key = value` format. It
supports `font-family`, `font-size`, `background`, `foreground`,
`cursor-color`, `selection-background`, `selection-foreground`, and
`palette = N=#rrggbb` for `N` from 0 to 15. A selected `options.json` theme
preset takes precedence over file colors; font values can still come from
the file. `theme_overrides` apply on top.

flowmux does not automatically read `~/.config/ghostty/config`. Import a
compatible file with `flowmux theme import PATH`, then use **Reload config**
in the command palette. A `theme = ...` line does not load another theme
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

`state.json` under `$XDG_STATE_HOME/flowmux` is managed by flowmux and is not a
user-editable configuration file. Runtime context variables include
`FLOWMUX_PANE_ID`, `FLOWMUX_SURFACE_ID`, `FLOWMUX_WORKSPACE_ID`,
`FLOWMUX_TAB_ID`, `FLOWMUX_SOCKET_PATH`, and optional
`FLOWMUX_BUNDLED_CLI_PATH`. `FLOWMUX_RUNTIME_DIR` can isolate a smoke run;
`FLOWMUX_LOG` sets the tracing filter for console and daily file logging,
for example `debug`. Log paths are described in [setup](setup.md#troubleshooting).
