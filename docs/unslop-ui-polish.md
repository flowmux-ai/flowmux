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
native font choices, Session Flow's cosmic identity and the Office's pixel-art identity.
Session Flow's star field, twinkles and node glow are intentional product identity.
Use 4px spacing steps
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
| Session Flow | Cosmic canvas, useful empty state, clear labels; retain graph navigation and fork controls | Native graph layout, hit testing, pan/zoom/drag and session routing |
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
- [x] Panels, dialogs and supporting surfaces
- [x] Session Flow and content-specific polish
- [x] Final regression and visual verification
- [x] Work-unit commits and final scope audit

### Shell pass

Selected workspace rows now paint an inset accent without changing content or
drag-target geometry. Tabs reserve the active stripe before selection. Control
spacing follows the existing compact desktop density, with explicit keyboard
focus rings. Working agents retain amber with a lighter fill than attention.
The native harness now installs the same bundled fallback icons as the app;
previous test captures showed missing-icon placeholders. The baseline also emits
system `gtk.css` parser warnings; the new check separately asserts that Flowmux's
own stylesheet produces no parsing errors.

### Panel pass

Files, worktrees, history, notifications, usage, SSH and review headers now share
compact control spacing. Keyboard focus remains distinct from selection; focused
panels use inset accents without shrinking their contents. Preferences keep their
existing control order and immediate-apply behavior. Browser controls retain their
32px height. Sidebar footer overflow exposes a scrollbar. Office pixel art and
workspace ordering are retained, with quieter captions and explicit focus cues.

Validation: full macOS native suite (`panels-native.log`) passed, including review
comment persistence, browser input, file operations, split/tab drag and terminal
wheel handling. Linux theme tests passed (20 tests, `linux-theme.log`).

### Footer reachability check

The additional review-only native check exposed a stale expectation that resizing
always scrolls the footer to its right edge. It failed with the original External
scrollbar policy too (`footer-original-policy.log`). The check now drives the real
wheel controller to both ends, verifies first/last button bounds and preserved
ordering, and repeats reachability after resize. The native overlay scrollbar and a scroll hint
make overflow discoverable without reducing button height. The complete review-only native run passed
(`footer-reachable.log`), including comment storage and handoff.

### Content and preferences pass

Session Flow retains its dark navy canvas and cyan/violet node meanings, centered
header, hit radii and all graph operations. The original cached glowing nodes and independent star animation are preserved;
a non-interactive empty-state overlay explains how relationships appear. Labels inherit the interface font. Presentation captures use
a separate mapped fixture so live shell presence polling cannot erase test data.

The editor keeps Monaco and its action order. Its surrounding dialogs, search rows
and status overlays now share spacing and smaller neutral shadows. Warning and
error text use separate light/dark values; the macOS WebKit test measures actual
computed foreground/background contrast against 4.5:1. Markdown spacing, heading
weight and link focus were aligned without changing the renderer or link routing.

Preferences retain the native grouped lists, immediate apply, reset behavior and
page order. Their repeated cookie-only checkbox descriptions were removed from
unrelated restoration settings; row labels identify each control. Theme/Update
fallback icons are bundled for hosts without the corresponding system icons.

Retained after review: Welcome's existing 24px margins and native navigation,
SSH/confirmation dialogs' safe action defaults, native image/Lottie controls,
provider artwork, terminal palettes, Office pixel scenes, and native keybinding,
skill and update controls. These already follow the chosen direction; their
behavior is covered by the platform gates rather than being rewritten.

Focused native checks passed: Session Flow entry points/layout/empty-state input,
all five preference pages in light and dark themes, browser chrome at narrow
widths, usage pins, zoomed-tab reorder, Office screen cache, and footer/review
interaction. The shipped editor bundle rebuilt and verified with 41 web tests.

### Final verification

- macOS regression: all 45 required phases passed, with records combined in
  `macos-phase-summary.json`. This was not one uninterrupted successful run:
  inherited Codex session identifiers were removed from the test environment,
  the update-banner expectation was corrected to `Restart Flowmux`, and the gate
  resumed at the failed phase. One WebDriver hover check lost native document
  focus; its standalone recheck and the resumed phase both passed unchanged.
  Earlier results remain in `macos-before-resume.log` and
  `macos-before-webdriver-resume.log`; the final segment is `macos-regression.log`.
- Linux regression: all 25 test groups are covered by passing results in an
  isolated Ubuntu 24.04 ARM64 VM checkout. The full run passed 24 groups,
  including all workspace tests (929 passed in the application binary), then
  exposed duplicate `Add comment` accessibility names in the SSH review.
  The line-comment entry is now `Line comment`, distinct from the save action.
  After rebuilding, the entire SSH fixture passed, including remote review,
  comments, history and feedback delivery (`linux-ssh-review-final.log`).
  The original gate's exit remains 1 in `linux-regression.log`; earlier groups
  were not redundantly rerun for this button-label correction. The final native
  macOS review check also passed (`review-final-native.log`).
  WebKit ran under the repository's scoped AppArmor profile, with its sandbox
  enabled. Production sources were copied to the VM rather than modifying its
  existing working checkout.
- Linux XTest pointer checks passed sequentially for the sidebar divider,
  split dividers and restored split dividers (`linux-pointer-drags.log`). All
  three report zero resize movement outside the resize cursor and successful
  adjacent terminal text selection. An earlier overlapping test invocation
  tripped the harness's unrelated-process lifecycle guard; the final run used
  no concurrent Linux GUI tests. Source hashes match the isolated checkout
  (`source-manifest.json`, `linux-source-parity.json`).
- The default editor-recovery phase includes `MACOS_EDITOR_STATUS_CONTRAST_OK`
  and retains renderer retry and deleted-file recovery checks. Actual WebKit
  computed warning/error contrast meets 4.5:1 in light and dark themes.
- Additional mapped Office testing passed 12 cycles with 32 residents and
  `OFFICE_STABILITY_OK` (`office-stress.log`). This checks short-run interaction
  and resource behavior, not indefinite leak freedom.
- Rendered review covers light/dark shell and all five preferences pages,
  browser chrome at 480/720/1200px and larger fonts, and populated/empty Session
  Flow. Captures are in `final/`, `session-flow.png` and `session-flow-empty.png`
  beneath the evidence directory.
- UI and Markdown crates pass Clippy for all targets with `native-smoke` and
  warnings denied (`clippy-ui.log`). Full-workspace Clippy stops at the unchanged
  redundant `let mut cmd = cmd` in `crates/flowmux-cli/src/main.rs:1494`
  (`clippy.log`); that unrelated code is outside this change.

No push, hosted CI, installation or release is part of this branch.
Physical mouse/trackpad testing across every hardware configuration has not been
performed; automated native input and the
rendered checks above define the verified boundary.


### Cosmic identity correction — 2026-11-14

The initial pass removed the star field and node glow too aggressively. The user
confirmed that the cosmic appearance is an intentional part of Session Flow.
Restored the original cached star field, eight sparse twinkles and glowing node
sprites, including drag/zoom priority, inactive/hidden pause and reduced-motion
handling. The centered header, readable labels and non-intercepting empty state
remain. `cosmic-restored/native.log` passes layout, entry-point, presentation and
`MACOS_SESSION_FLOW_STARS_CACHE_MOTION_OK` checks. The restored populated and empty
views are captured under `cosmic-restored/` in the evidence directory.


### Aesthetic revision — 2026-11-14

Audit of the rendered screen: General preferences repeated one heavy card for every
setting, selected tabs blended into their bar, and footer tools sat against the
window edge without a visual group. The revision addresses those specific issues.

- Product: a desktop workspace for operating and following coding agents.
- Character: precise, calm, alive.
- World: an observatory around active work.
- Moments: Session Flow's cosmic graph and the Office's pixel scenes.
- Signature: a restrained accent identifying the active workspace and session.

General preferences now use six named groups with a shared surface and internal
hairlines. All 22 controls retain their original order, labels, values and apply
callbacks. Tabs have a rounded inset silhouette and a clearer selected surface;
pane tools form a quieter group beside them. The active workspace has a neutral
surface behind its existing accent. Sidebar footer tools sit in a padded frame
and retain their original sequence and horizontal scrolling. Existing themes,
terminal palettes, browser content, provider colors and Office artwork remain.

The native presentation check now verifies the complete setting order, label/control
associations and the last setting's reachability in both light and dark themes.
Validation under `target/unslop-evidence/aesthetic-pass/`:

- `native-full.log`: complete default macOS native harness passed.
- `chrome-captures.log`: light/dark shell, selection geometry, all five options
  pages and top/bottom preference captures passed.
- `REVIEW.log`: footer scrolling/resize reachability and review interactions passed.
- `BROWSER_CHROME.log`, `ZOOM_REORDER.log`, `OPTIONS_MERGE.log`: narrow browser
  chrome, zoomed tab reordering and concurrent settings persistence passed.
- `theme-tests.log`: 20 theme tests passed; `clippy.log`: UI all-target Clippy with
  native-smoke and warnings denied passed. Formatting and diff checks passed.
- `linux-focused.log`: 18 preferences tests, rebuilt GUI and all three XTest divider
  cases passed. Each divider case reports zero resize outside the resize cursor
  while adjacent terminal text selection succeeds.

Compare `final/options-general-one-dark.png` with
`aesthetic-pass/options-general-one-dark.png`, and the corresponding
`shell-one-dark.png` images. The light-theme and bottom-of-settings captures are
also retained. This revision uses focused regression checks; it does not claim a
new full macOS/Linux CI gate run, hosted CI, release build or app installation.
