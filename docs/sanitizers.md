<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Memory sanitizer checks

Linux x86-64 tests use pinned `nightly-2026-09-21` and a separate
`target/sanitizers` directory. Install the normal [native dependencies](setup.md),
Xvfb, D-Bus, `llvm-18` (for symbolized reports), and the sanitizer toolchain:

```sh
rustup toolchain install nightly-2026-09-21 --profile minimal --component rust-src
bash scripts/test-sanitizers.sh headless
bash scripts/test-sanitizers.sh lottie -- --test-threads=1
bash scripts/test-sanitizers.sh gui -- --test-threads=1
```

The headless command selects the workspace's default members; the GUI command
selects `flowmux` and `flowmux-md-viewer`. Extra arguments are passed to `cargo test`, so a
focused check can use `gui --bin flowmux closing_terminal -- --test-threads=1`.
GUI tests run under X11/Xvfb and a private D-Bus session. Both commands isolate
XDG state and remove inherited `FLOWMUX_*` variables; they do not restart or
control an existing window.

AddressSanitizer instruments Rust dependencies and the standard library
(`-Zbuild-std`). LeakSanitizer runs at normal process exit for headless tests
and the focused native Lottie tests (which do not initialize GTK).
Findings fail the command; no application leak suppressions are installed. The GitHub
`Sanitizers` workflow runs the same commands on pushes and pull requests.

Full GUI leak checking remains an explicit diagnostic command:

```sh
bash scripts/test-sanitizers.sh gui-leaks -- --test-threads=1
```

The initial full GUI run passed 690 assertions but failed its exit leak check:
1,199,677 bytes in 32,923 allocations, mostly Fontconfig (1,080,024 bytes),
with additional GTK/GLib/WebKit and native TLS allocations. This has not been
established as a clean GUI leak baseline. A separate GTK-only C label/window
probe also reported 31,304 bytes in 1,169 Fontconfig/Pango allocations without
any flowmux code; this does not account for every full-suite report.
`gui` therefore disables only leak
checking; ASan invalid-access checks and GTK widget-release assertions still
run. The strict headless and Lottie gates do not inherit that exception.
Use `gui-leaks` to investigate remaining ownership and native teardown reports;
do not treat the GUI ASan gate as evidence that GUI leaks are absent.

System GTK, VTE, WebKit, ThorVG and other native libraries are not rebuilt with
ASan. Allocator interception may catch errors crossing their boundaries, but
unchecked native memory accesses and WebKit helper processes are outside full
coverage. These checks also do not establish an RSS/PSS ceiling, bounded cache
growth, or macOS memory safety. GTK weak-reference lifecycle tests remain
necessary: a reachable reference cycle can retain widgets without appearing
as an unreachable leak to LSan.
