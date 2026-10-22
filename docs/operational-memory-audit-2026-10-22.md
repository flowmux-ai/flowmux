<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Operational memory audit — 2026-10-22

This follow-up excludes ThorVG. It investigates terminal output, workspace and
panel lifetimes, Code Review, editor tabs, notifications, usage collection, and
Markdown previews. Three input-driven OOMs were reproduced in real applications.
The lifecycle experiments did not establish an unreleased-object leak.

## Method and scope

Baseline: `69e6ad08`, Linux debug binaries, VTE, WebKitGTK, Xvfb and the Cairo GTK
renderer. Every GUI used private XDG directories, D-Bus and an explicit owned
socket. The user's running GUI was left untouched. Test services had a 512 MiB,
768 MiB or 1 GiB cgroup limit and no swap; these are controlled memory-pressure
reproductions, not measurements of the user's original crash.

Samples record `/proc/PID/smaps_rollup`, file descriptors, threads, child
processes and, in later runs, cgroup memory counters. RSS includes shared pages;
do not add process RSS values or equate RSS with cgroup memory. Concurrent runs
also change PSS attribution. Kernel `CONSTRAINT_MEMCG` and killed-PID records,
not exit status alone, establish the OOMs below.

Local artifacts are under `target/ops-memory-audit-20261022/`: `probe.py`,
`launch.py`, per-scenario logs, `summary.json`, and `oom-kernel.log`. Each log
names a private `/tmp/fm-gui-*` directory containing screenshots, GUI logs and
memory samples. These generated artifacts are not committed. The harness reuses
`scripts/test-ssh-workspace-gui.py`; the probe also uses the sampler retained in
`target/memory-audit-20261022/soak.py` from the preceding investigation.

## Confirmed findings and changes

### 1. Code Review expands small, high-line-count diffs into excessive GTK storage

- Trigger: an untracked file containing `x\n` 2,700,000 times (5,400,000 bytes).
  Its patch remains below the existing 8 MiB Git output limit.
- Reproduction: create/focus its workspace and open Code Review with Ctrl+Alt+E.
  Main-GUI RSS rose from 188.9 MiB to at least 1,070.5 MiB; the 1 GiB service
  exhausted its memory budget. The kernel killed `flowmux` PID 757106.
- Cause: `Snapshot::patch_with_context` accepted the byte-bounded patch,
  `parse_patch` allocated a string per line, and `ReviewWindow::show_page` inserted
  every row with tags and line numbers into a GTK TextBuffer. File-list
  virtualization does not virtualize the diff buffer.
- Change: reject patches above 100,000 lines before allocating parsed rows.
  Existing error UI shows the limit and directs the user to a smaller change,
  Git, or an editor. No patch or file is truncated or modified.
- Regression: an approximately 300 KiB patch with 100,001 additions must fail;
  99,000 additions still load with correct final line numbers. The rejection
  test failed against the baseline implementation before the fix.

Sources: `crates/flowmux-vcs/src/review.rs`,
`crates/flowmux-vcs/tests/review.rs`, `crates/flowmux/src/ui/review_window.rs`.

### 2. Usage collection allocates an entire JSONL record before filtering it

- Trigger: a generated Claude history file with one valid, old `user` record
  containing 512 MiB of text. It contributes no tokens to today's usage.
- Reproduction: simply start Flowmux with that isolated `CLAUDE_CONFIG_DIR`.
  The usage cache warms at startup, before opening the popover. In a 768 MiB
  service, main-GUI RSS reached 702.2 MiB and returned to about 189 MiB after
  collection. In a 512 MiB service the kernel killed `flowmux` PID 758919 before
  the first settled baseline sample.
- Cause: `sum_transcript_reader` used `BufRead::lines()`, which allocated the
  full line before deserializing and checking record type/date. This is a peak
  allocation, not evidence of a leak. The intentionally large synthetic record
  establishes the missing bound; ordinary small records do not imply this cost.
- Change: read at most 16 MiB plus one byte per record. Oversized records make
  the token field unavailable with an explicit error; they are not silently
  skipped into an incorrect partial total. Normal filtering, deduplication,
  malformed-line handling and I/O errors remain intact.
- Regression: a cursor confirms reading stops at limit + 1, and a tree-level
  test confirms an oversized record produces an invalid-data result. The
  bounded-reader test failed against the baseline before the fix.

Sources: `crates/flowmux/src/usage/claude.rs`,
`crates/flowmux/src/ui/usage_popover.rs`.

### 3. Markdown preview parsing amplifies source text without a read budget

- Trigger: 600,000 ordinary list items, each containing bold text, a link and
  inline code: 35,888,890 bytes (34.2 MiB). No images or ThorVG are involved.
- Reproduction: open the file in `flowmux-md-viewer`. Viewer RSS reached
  991.7 MiB during parsing; the kernel killed viewer PID 760862 in the 1 GiB
  service. The companion test GUI was also killed in this same shared cgroup.
  This does not establish that a viewer normally owns the main GUI or that a
  viewer crash always terminates it.
- Controls: 100,000 items (5.6 MiB) completed, with viewer RSS peaking at
  413.8 MiB. 300,000 items (17.1 MiB) completed under pressure, with viewer RSS
  peaking at 978.8 MiB. WebKit children add further memory after parsing.
- Cause: whole-file reading followed by Comrak AST construction, HTML creation
  and WebKit DOM loading. The AST allocation can exhaust memory before any
  browser document is ready.
- Change: enforce a 1 MiB source budget before parsing, including bounded file
  reads. Both file previews and direct document rendering use the check.
  The existing error page explains the limit; ordinary text editing remains
  available. The budget intentionally limits preview functionality rather
  than attempting to estimate arbitrary Markdown/DOM expansion after allocation.
- Regression: exact byte boundary, over-limit in-memory input, UTF-8 split at
  the read boundary, and successful rendering after a large file becomes small.

Sources: `crates/flowmux-md-viewer/src/render.rs`, `README.md`.

## Lifecycle and negative results

| Scenario | Observation |
| --- | --- |
| 40 workspace create/focus/close cycles, with review, overview and global terminal search | After warm-up, RSS stayed near 216.6 MiB; cycle 5 to 40 changed by less than 0.3 MiB. FDs stayed at 23 and only the original shell/proxy remained. |
| 64 MiB terminal output without newlines, default 5,000-line scrollback and minimap enabled | Main-GUI RSS 194.0 → 197.7 MiB. Completion marker verified; clearing history did not reveal unbounded growth. |
| Open/close a 500,000-line Code Review 12 times | RSS rose during warm-up, fluctuated between about 400 and 451 MiB, and settled at 429.5 MiB. FDs stayed at 23. This retained RSS is not by itself proof of a reference leak; there was no steady per-cycle slope. |
| Editor: 500,000-line / 4.8 MiB and 2,700,000-line / 5.1 MiB files | Large WebKit memory cost, but no OOM at the tested 1 GiB budget. Closing each editor tab removed its WebKit descendants. Main-GUI FDs remained 36 versus 23 initially; a single open/close does not establish whether those process-wide resources accumulate. |
| 2,000 notifications in 20 retain/clear batches | Retention stayed at 50 entries. Main-GUI RSS 187.7 → 191.0 MiB after settling; no per-batch growth trend. FDs 23 → 24, then stable. |

The preceding audit also exercised 30 terminal output/close cycles and 20
browser snapshot/close cycles. Those are earlier evidence, not new runs here.

Static checks found existing bounds in the IPC reader (1 MiB messages,
connection/mutation admission limits), GTK command queues, OSC extraction
(64 KiB), notification retention (50 entries), terminal persistence, global
terminal search, and coalesced editor recovery. Their mere use of vectors,
channels or background tasks was not treated as a memory leak.

## Remaining investigation limits

- Usage transcript workers use `spawn_blocking`; a UI timeout does not cancel
  an already running disk scan. Slow/network filesystems and overlapping
  retries need a separate cancellation experiment. The line-size fix does not
  claim to solve that hypothesis.
- State restoration and saving still materialize whole JSON state and may
  temporarily duplicate it. Many saved workspaces warrant a dedicated scale
  test; no state-load OOM or persistent backlog was proved here.
- Expanded file-browser directories are read before the visible-row limit,
  and obsolete directory reads may finish after a generation change. Slow,
  huge directory churn remains a candidate, not a confirmed leak.
- No macOS run, GPU-memory measurement, multi-day soak, large browser DOM
  stress, SSH reconnect soak, or heap-level proof of all GTK ownership paths.
- The Markdown source budget is not a total WebKit-memory guarantee. Embedded
  resources, HTML and diagrams can consume memory independently of source size.
  A 17,000-item document (991,890 bytes), below the new limit, still OOM-killed
  WebKit PID 784674 when the companion GUI, viewer and WebKit shared a 512 MiB
  budget. The same input rendered normally at 1 GiB, with a measured cgroup
  peak of 548.3 MiB. This remaining low-memory rendering failure is not fixed
  by the source-read guard and is not a demonstrated leak.

## Final validation

Separate implementation commits:

| Commit | Improvement |
| --- | --- |
| `99de3db7` | Bound diff rows before parsing/rendering; document the limit. |
| `b5defb44` | Bound transcript records and report incomplete collection explicitly. |
| `7ed17277` | Bound Markdown preview reads and document rendering; document the limit. |

All three original OOM inputs were retried in isolated real applications:

| Run | Budget | Result after the change |
| --- | --- | --- |
| `review-fixed-1` | 1 GiB | Visible 100,000-line error; GUI sampled peak RSS 204.7 MiB, kernel VmHWM 207.6 MiB; IPC remained responsive and shutdown was normal. |
| `usage-fixed-1` | 512 MiB | Visible 16 MiB record error; GUI VmHWM 204.6 MiB including startup; normal shutdown. The service reached its cgroup limit while writing/reclaiming the synthetic log's page cache, with zero OOM events. |
| `markdown-fixed-1` | 1 GiB | Visible 1 MiB preview error; viewer sampled peak RSS 182.2 MiB; companion GUI remained responsive. |

Each run's final cgroup `oom` and `oom_kill` counters were zero. Separate
recovery runs changed the test-owned input to a small valid document in the
same application: review refreshed to two lines, usage displayed exactly
`Tokens today 7`, and the Markdown watch reloaded to a normal formatted page.
Screenshots were inspected for both the error and recovered states. The
991,890-byte Markdown control also rendered normally in the 1 GiB service.

Validation:

- Affected packages (`flowmux`, `flowmux-vcs`, `flowmux-md-viewer`) under private
  XDG/D-Bus/Xvfb with Cairo and serial tests: **876 passed, 4 ignored, 0 failed**.
  Includes CommonMark/GFM specifications, viewer CLI tests and review tests.
- The first non-isolated parallel run had two GUI mapping/focus assertion
  failures (`shortcut_close_restores_focus_to_the_previous_widget` and
  `surface_tab_releases_widget_and_callbacks`). Both passed in the complete
  isolated rerun without additional product changes.
- `cargo fmt --all -- --check`, workspace/all-target Clippy with `-D warnings`,
  and `git diff --check` passed. GUI and viewer binaries built successfully.
- No full coverage gate or new sanitizer run was performed for this follow-up.
  Earlier audit results must not be counted as new validation of these changes.
- All operational probe services stopped. The protected GUI PID 746615 retained
  its original start token `62031502`; it was not restarted or replaced.
