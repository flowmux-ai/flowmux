<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Theme palette review

Reviewed 2026-10-03 against the bundled theme files and resolved Default theme.
There are 15 built-in choices, including Default. User overrides are intentionally
not normalized: they continue to take precedence.

## Useful additions

| Theme | Intended use | Color distribution |
| --- | --- | --- |
| GitHub Dark | Neutral dark canvas for code, diffs and command output | Distinct red/green/yellow/blue/magenta/cyan families; all 12 chromatic ANSI slots exceed 4.5:1. Black/bright black remain subdued. |
| Gruvbox Light | Warm cream canvas for a light desktop | FlowMux adaptation: dark neutral slots, deeper ANSI accents for readable logs. All 12 chromatic slots exceed 4.5:1; neutral 7/8 remain subdued. |
| FlowMux Contrast Dark | Dense colored logs on a dark canvas | Original palette; all 16 ANSI foregrounds exceed 4.5:1, body text exceeds 7:1. Bright slots are lighter and all 16 colors are distinct. |
| FlowMux Contrast Light | Dense colored logs on a light desktop | Original palette; all 16 ANSI foregrounds exceed 4.5:1, body text exceeds 7:1. Bright slots remain distinguishable without becoming pastel. |

GitHub Dark uses the [GitHub theme](https://github.com/primer/github-vscode-theme)
foreground and [Primer Primitives 7.10.0](https://unpkg.com/@primer/primitives@7.10.0/dist/json/colors/dark.json)
ANSI values. Gruvbox Light starts from the [upstream palette](https://github.com/morhetz/gruvbox/blob/master/colors/gruvbox.vim),
with FlowMux's contrast adjustments identified in the theme file. Credits and
MIT terms are retained in the bundled notices.

## Corrections to existing themes

| Theme | Before | After |
| --- | --- | --- |
| Solarized Light | Body 4.13:1; selected text 4.39:1; selected background barely different from canvas (1.14:1) | Body 4.99:1; selected text 5.61:1; selection/canvas 2.48:1. Uses darker Solarized body text and a stronger selection. |
| Solarized Dark | Selection/canvas 1.15:1 | Selection/canvas 2.79:1, selected text 4.99:1. Uses stronger Solarized selection colors. |
| Catppuccin Latte | Selected text 3.69:1; cursor 2.34:1 | Selected text 4.77:1; cursor 4.79:1. Darker selected text and a mauve cursor. |

These corrections retain the existing ANSI palettes and preset IDs.

## Assessment of retained palettes

| Theme | Assessment |
| --- | --- |
| Default | Strong body contrast. Red and neutral gray ANSI text is softer; Contrast Dark is better for small colored logs. |
| One Dark | Balanced coding palette; normal/bright hue pairs mostly repeat. ANSI black blends into the canvas, and red is marginal for small text. |
| Dracula | Clear saturated accent families and distinct bright variants. Neutral black/bright black are intentionally subdued. |
| Nord | Restrained cool palette. Red and magenta need attention in small logs; most normal/bright accents repeat. |
| Gruvbox Dark | Warm differentiated hues. Normal red, blue and purple are low contrast; retain for its aesthetic rather than high-contrast logs. |
| Catppuccin Mocha | Pastel accents contrast well on the dark canvas. Bright chromatic slots repeat the normal slots; dim grays are weak. |
| Tokyo Night | Cool background with clear chromatic accents. Bright chromatic slots repeat the normal slots; grays are intentionally dim. |
| Solarized Dark | Deliberately limited luminance range. Its bright bank includes base neutrals rather than uniformly brighter hues; several ANSI text colors remain weak. |
| Solarized Light | Corrected body and selection are readable. Many original ANSI colors are too light for small text, including white matching the canvas; use Contrast Light for colored logs. |
| GitHub Light | Strong main text and dark regular accents. Bright blue, purple, cyan and white have weaker contrast. |
| Catppuccin Latte | Corrected selection/cursor; main text is strong. Pastel yellow, green, magenta and light neutral slots remain weak; use Contrast Light for colored logs. |

The original palettes' neutral slots and repeated bright hues are documented
rather than globally recolored, which would change existing users' TUI appearance.
The preview includes both ANSI banks so these tradeoffs can be inspected directly.

## Measurement and regression gates

Ratios use linearized sRGB relative luminance `(lighter + 0.05) / (darker + 0.05)`.
The [W3C text contrast reference](https://www.w3.org/WAI/WCAG22/Understanding/contrast-minimum.html)
provides the 4.5:1 ordinary-text benchmark. FlowMux's test gates are:

- Every built-in preset: body and selected text >= 4.5:1; cursor/canvas >= 3:1.
- Selection/canvas >= 1.25:1 is a local visibility heuristic, not a WCAG criterion.
- New GitHub Dark and Gruvbox Light: all 12 chromatic ANSI slots >= 4.5:1.
- FlowMux Contrast pair: body >= 7:1, all 16 ANSI foregrounds >= 4.5:1,
  no duplicate colors, and each bright slot lighter than its normal counterpart.

This evaluates foregrounds against the default canvas, not every possible ANSI
foreground/background combination or app-supplied truecolor. It is not a claim
of whole-application accessibility or color-vision-deficiency certification.
Default's selection measurement uses the terminal's inverse-color fallback;
the editor has its own translucent fallback. Ratios are rounded here, but test
thresholds use unrounded values. ANSI indices 0-7 are normal, 8-15 bright.

Reproduce the table and gates:

```sh
cargo test -p flowmux --bin flowmux theme::tests::preset_contrast_audit -- --nocapture
```

| Theme | Text | Selection text | Selection vs canvas | Cursor | ANSI below 4.5 | Repeated normal/bright accents |
| --- | ---: | ---: | ---: | ---: | --- | --- |
| Default | 14.00 | 14.00 | 14.00 | 14.00 | 0 (2.32), 1 (3.77), 8 (3.73), 9 (3.36) |  |
| One Dark | 6.57 | 4.58 | 1.43 | 4.33 | 0 (1.00), 1 (4.38), 8 (2.32), 9 (4.38) | 1/9, 2/10, 4/12, 5/13, 6/14 |
| Dracula | 13.36 | 8.59 | 1.56 | 13.36 | 0 (1.11), 8 (3.03) |  |
| Nord | 9.25 | 5.46 | 1.69 | 9.25 | 0 (1.24), 1 (3.05), 5 (4.41), 8 (1.69), 9 (3.05), 13 (4.41) | 1/9, 2/10, 3/11, 4/12, 5/13 |
| Gruvbox Dark | 10.75 | 6.43 | 1.67 | 10.75 | 0 (1.00), 1 (2.69), 4 (3.48), 5 (3.48), 8 (4.02), 9 (4.29) |  |
| Catppuccin Mocha | 11.34 | 4.62 | 2.46 | 12.95 | 0 (1.80), 8 (2.46) | 1/9, 2/10, 3/11, 4/12, 5/13, 6/14 |
| Tokyo Night | 10.59 | 5.64 | 1.88 | 10.59 | 0 (1.05), 8 (1.91) | 1/9, 2/10, 3/11, 4/12, 5/13, 6/14 |
| Solarized Dark | 4.75 | 4.99 | 2.79 | 4.75 | 0 (1.15), 1 (3.25), 4 (4.08), 5 (3.30), 8 (1.00), 9 (3.26), 10 (2.79), 11 (3.37), 13 (3.43) |  |
| Solarized Light | 4.99 | 5.61 | 2.48 | 4.99 | 1 (4.29), 2 (2.97), 3 (2.98), 4 (3.41), 5 (4.21), 6 (2.93), 7 (1.14), 9 (4.27), 11 (4.13), 12 (2.93), 13 (4.06), 14 (2.48), 15 (1.00) |  |
| GitHub Light | 14.65 | 9.66 | 1.52 | 14.65 | 12 (3.39), 13 (3.24), 14 (3.61), 15 (3.04) |  |
| Catppuccin Latte | 7.06 | 4.77 | 1.91 | 4.79 | 2 (2.96), 3 (2.31), 4 (4.34), 5 (2.34), 6 (3.31), 7 (1.91), 8 (4.37), 10 (2.96), 11 (2.31), 12 (4.34), 13 (2.34), 14 (3.31), 15 (1.61) | 1/9, 2/10, 3/11, 4/12, 5/13, 6/14 |
| GitHub Dark | 16.02 | 7.19 | 2.23 | 7.49 | 0 (2.28), 8 (4.12) |  |
| Gruvbox Light | 10.22 | 6.76 | 1.51 | 5.40 | 7 (4.29), 8 (3.24) |  |
| FlowMux Contrast Dark | 17.54 | 8.21 | 2.14 | 14.88 |  |  |
| FlowMux Contrast Light | 15.51 | 10.86 | 1.45 | 7.52 |  |  |

Native verification in `macos_native` switches through every preset in an isolated
window, checks persisted selection and rendered backgrounds, and checks each
ANSI color in VTE's rendered HTML. The same run covers custom resets, browser
navigation, editor/terminal focus, IPC, and unsaved-document close cancellation.
The original running FlowMux/Codex window is not closed or restarted.
