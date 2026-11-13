<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Flowmux UI polish

Branch: `design/unslop-ui-polish`, starting at `ad4cd335`.

Design method: [unslop by Damir Sokolovsky](https://github.com/heydamir/unslop),
CC BY 4.0. This review applies its spacing, hierarchy, quiet chrome and
purposeful motion principles to the existing native application.

## Direction and constraints

- Product: a developer workspace for operating and following AI coding sessions.
- Character: calm, precise, responsive.
- World: a working developer's desk and status instruments.
- Moments: an agent needing attention and the selected session's relationships.
- Signature: the session connection line, with consistent focus and status cues.

Keep existing terminal palettes, configured focus colors, provider identities,
native font choices and the Office's pixel-art identity. Use 4px spacing steps
with 2px optical adjustments, quiet surfaces, legible text and consistent controls.
Do not change command routing, action order, persistence, permission handling,
terminal content, split geometry or drag/drop semantics to achieve a visual change.
Keep keyboard focus visible and keep all existing controls reachable.

## Coverage ledger

Each family needs a source review, the relevant changes (or an explicit reason to
retain it), and appropriate verification before the work is complete.

| Family | Planned review/change | Verification |
| --- | --- | --- |
| Window, workspace sidebar, tabs and pane tools | Consistent spacing, stable selected-row geometry, neutral chrome, distinct focus | Theme tests; native light/dark; tab drag, reorder, zoom and split checks |
| Agents, notifications, AI Usage | Reserve broad emphasis for attention; align meta and controls; preserve status meanings | Agent/status tests; notification and usage native checks |
| Files, worktrees, session history, terminal search | Consistent panel headers, row and keyboard selection states | Native panel actions, file operations, search and session checks |
| Session Flow | Quiet canvas, useful empty state, clear labels; retain graph navigation and fork controls | Native graph layout, hit testing, pan/zoom/drag and session routing |
| Agent Office and workspace overview | Keep meaningful content; quiet and align controls, captions and overlays | Native Office interaction/cache and overview selection/close checks |
| Browser, task controls, dialogs and media | Normalize control spacing, radii and focus; retain responsive toolbar | Browser chrome minimum-width tests; native input, dialogs and media checks |
| Editor and Code Review | Consistent surrounding chrome, readable review comments and file rows | Editor tests/assets; native edit/save/focus, review/comment/handoff checks |
| Preferences, themes, keybindings, skills and updates | Consistent setting rows, grouping, action hierarchy and page spacing | Native options/theme/skills/update checks; preserve immediate apply and resets |
| Welcome, SSH, menus and confirmation dialogs | Clear copy, consistent action treatment and spacing | Existing routing and dialog tests, live spot checks |
| Markdown, images and Lottie viewers | Consistent controls and theme-compatible content framing | Markdown and image-viewer native checks |

## Evidence

- Existing untracked user files are outside this change.
- Baseline and subsequent logs are retained under `target/unslop-evidence/`.
- Completion requires the full relevant macOS regression gate, focused interaction
  checks, rendered light/dark review and an explicit account of Linux verification.
- Test expectations may change for intentional visuals; interaction assertions
  must remain, including ordering, drag targets, focus and persisted settings.

## Progress

- [x] Inventory and dedicated branch
- [x] Baseline native checks (`baseline-native.log`, `MACOS_NATIVE_SMOKE_OK`)
- [x] Common shell and status styling (20 theme tests; native light/dark selection geometry)
- [ ] Panels, dialogs and supporting surfaces
- [ ] Session Flow and content-specific polish
- [ ] Final regression and visual verification
- [ ] Work-unit commits and final scope audit

### Shell pass

Selected workspace rows now paint an inset accent without changing content or
drag-target geometry. Tabs reserve the active stripe before selection. Control
spacing follows the existing compact desktop density, with explicit keyboard
focus rings. Working agents retain amber with a lighter fill than attention.
The native harness now installs the same bundled fallback icons as the app;
previous test captures showed missing-icon placeholders. The baseline also emits
system `gtk.css` parser warnings; the new check separately asserts that Flowmux's
own stylesheet produces no parsing errors.
