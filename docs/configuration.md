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
`system_notifications_enabled`, `agent_bar_mode`, `cursor_blink`,
`cursor_blink_interval_ms`, `font_family`, `font_size`,
`agent_notification_target`, `theme`, `theme_overrides`, and `keybindings`.
`default_shell` selects the command for new tabs; a per-tab IPC `shell` takes
precedence, then `$SHELL` is used. Invalid commands fall back safely.
`agent_bar_mode` switches Agent Activity from the resizable lower side-panel
area to the compact bottom bar.

`terminal_minimap_enabled` defaults to `true`. It includes the current screen
after commands such as `clear`; setting it to `false` restores the standard
scrollbar. `terminal_minimap_width` defaults to `50` pixels and is clamped to
`12..=96`. `terminal_minimap_opacity` defaults to `20` percent and is clamped
to `0..=100`. All three settings apply to open and new tabs.

`zoom_percent` applies to terminal and browser surfaces. Terminal zoom is
rounded to a whole-point font size instead of using VTE's fractional font
scale, avoiding GTK text-damage artifacts during cursor blink. Editors keep
their own zoom per tab and use the last changed editor zoom as the default for
new tabs. The resolved theme plus `font_family` and `font_size` overrides are
applied live to both terminal and editor text; editor selection and cursor
colors follow the same theme.

## Ghostty configuration

When present, `~/.config/ghostty/config` supplies `font-family`, `font-size`,
`theme`, `background`, `foreground`, `cursor-color`, `selection-background`,
`selection-foreground`, and `palette = N=#rrggbb`. Unknown keys are retained
for diagnostics but do not alter flowmux behavior.

## cmux.json

Project-local `cmux.json` supports `name`, `env`, and `commands`. Each command
has `id`, `label`, `run`, optional `cwd`, `target` (`focused_pane`,
`split_down`, `split_right`, or `new_surface`), and `confirm`.

## State and environment

`state.json` under `$XDG_STATE_HOME/flowmux` is managed by flowmux and is not a
user-editable configuration file. Runtime context variables include
`FLOWMUX_PANE_ID`, `FLOWMUX_SURFACE_ID`, `FLOWMUX_WORKSPACE_ID`,
`FLOWMUX_TAB_ID`, `FLOWMUX_SOCKET_PATH`, and optional
`FLOWMUX_BUNDLED_CLI_PATH`. `FLOWMUX_RUNTIME_DIR` can isolate a smoke run;
`FLOWMUX_LOG` selects a log file. `NO_COLOR=1` disables CLI colour.
