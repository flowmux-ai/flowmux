<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Changelog

All notable changes are recorded here in release order. This document does not
change the crate version by itself.

## [Unreleased]

### Added

- Added Antigravity CLI process detection, lifecycle hooks, session restore,
  browser skill installation, and a native-color agent icon.

## [0.9.0] - 2026-08-10

### Added

- Added live workspace previews after a sustained sidebar hover, refreshed in
  place without switching away from the active workspace.
- Added a bounded terminal scrollback minimap with color sampling, viewport
  navigation, clear-state preservation, and an `ALT` indicator that disables
  unsafe history navigation while an alternate-screen application is active.
- Added an optional editor minimap and live editor-name refresh.
- Added stable naming and messaging discovery for plain Claude Code sessions.

### Performance

- Rendered workspace previews directly at their maximum `384x240` display size
  instead of allocating full-workspace textures every 250 ms.
- Bounded styled scrollback before XML parsing, removed avoidable editor and
  surface clones, and kept minimap state fixed at 128 samples per pane.

### Fixed

- Stabilized pane splitting, tab drag reparenting, background workspace focus,
  and keyboard focus/input in newly opened windows.
- Preserved styled terminal scrollback across restarts and minimap colors after
  clearing a terminal.
- Avoided stale IBus fallback input and expired stale usage limits even when a
  provider refresh fails.
- Improved release-tarball installation, Linux dependency bootstrapping, and
  ThorVG installation setup.

## [0.7.9] - 2026-07-17

- Consolidated packaging metadata and improved install-origin update checks.
- Added crash diagnostics persistence and safer multi-window state merging.

## [0.7.x]

- Added terminal shell selection, scrollback search, pane zoom, scrollback
  configuration, notification cues, and new-tab IPC.
- Restored release test gates and aligned integration capability contracts.

## [0.6.x]

- Expanded browser/terminal pane workflows, agent notifications, and CLI
  automation contracts.

## [0.5.x]

- Added persistent workspaces, browser session handling, and configuration
  loading improvements.

## [0.4.x]

- Improved pane layout, terminal interaction, and desktop integration.

## [0.3.x]

- Added the in-app browser surface and initial agent workflow plumbing.

## [0.2.x]

- Introduced the GTK terminal application, workspace model, and IPC daemon.

## [0.1.0]

- Initial public flowmux terminal and workspace implementation.
