<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Alacritty terminal patch

This is the library source of `alacritty_terminal` 0.26.0 from crates.io,
upstream commit `94e7c8874e526b1e67b349d9ba30ddf81669119e`, crate checksum
`bda177466b9524d59f1b12f0dd30b68696788e9992a7e959021c4a0ed96fcf59`.
Upstream source remains under Apache-2.0; see `LICENSE-APACHE`.

`grid/row.rs` adds `self.inner.shrink_to_fit()` after splitting a row. Reflow
from 500 columns to 2 otherwise retains almost the old width in each new row,
using 324MiB for Flowmux's 100,000-cell history before projections or metadata.
Freeing the retained capacity preserves the existing single-step reflow and
its text, cursor and primary/alternate-screen behavior.

`grid/resize.rs` calls `self.raw.truncate()` at the end of `Grid::resize` when the
grid keeps no scrollback (`max_scroll_limit == 0`). A resize otherwise leaves
offscreen row caches with their cell payloads on the alternate screen; with
maximum-width rows on both screens this exceeded Flowmux's per-execution memory
reservation. Visible rows and the primary screen's scrollback are kept.

`term/mod.rs` adds the read-only accessor `Term::scroll_region`. Flowmux restores
the region in a view's terminal after redrawing it; upstream keeps the field
private. Remove it when upstream exposes the region.

All other library source files are unchanged. The normalized Cargo manifest
omits the reference-test target because its 46MiB recording corpus is not
vendored. Library unit tests remain available. Flowmux's isolated narrowing
and widening memory regressions cover the changed behavior.

Remove each change when a released upstream version frees the same storage and
passes those regressions. Do not replace the reflow change with intermediate
resizes, which can change reflow and cursor behavior.
