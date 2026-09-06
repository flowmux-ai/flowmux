<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Specification fixture provenance

- `commonmark-0.31.2-spec.json` is an unmodified snapshot of the CommonMark
  0.31.2 specification examples from
  <https://spec.commonmark.org/0.31.2/spec.json>. The CommonMark specification
  is Copyright (C) 2014-16 John MacFarlane and licensed under
  [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/).
- `gfm-spec.txt` is an unmodified snapshot of the GitHub Flavored Markdown
  0.29-gfm specification from
  <https://raw.githubusercontent.com/github/cmark-gfm/0.29.0.gfm.13/test/spec.txt>.
  It is based on the CommonMark specification by John MacFarlane and licensed
  under [CC BY-SA 4.0](https://creativecommons.org/licenses/by-sa/4.0/).

The snapshots have not been modified by flowmux.

`flowmux-spec-differences.json` records flowmux's reviewed, input-specific
expected output where its enabled extensions differ from those specifications.
It is adapted from the fixtures above, under the same CC BY-SA 4.0 license.
Each entry explains the difference: heading anchors, front matter, full code
info strings, tag filtering, underline, task lists, extended autolinks, or the
modern CommonMark emphasis rules absent from the old GFM snapshot. The only
accepted output alternatives are the two orders of code metadata attributes.
All other examples compare directly against the upstream expected HTML.

These are regression expectations, not a claim of strict CommonMark/GFM
conformance. Review the input, intended extension behavior, and expected HTML
when updating them; do not regenerate them automatically to make a failure pass.
