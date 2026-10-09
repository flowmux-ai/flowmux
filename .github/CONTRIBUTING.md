<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Contributing

Build and test with the commands in the [setup guide](../docs/setup.md#build-from-source).
Keep changes focused, run
`cargo fmt --all`, Clippy with warnings denied, and the locked workspace test
suite before committing. Contributions are licensed under GPL-3.0-or-later.

Enable the versioned pre-push hook once per clone:

```sh
git config --local core.hooksPath .githooks
```

Check for an existing `core.hooksPath` or `.git/hooks/pre-push` before enabling
it; preserve any existing hooks. The hook requires a clean working tree
(including untracked files) and validates the checked-out commit being pushed.
Push other commits separately after checking them out. Ref deletions need no
checks. It runs formatting, locked Clippy for all workspace targets, and the
headless workspace tests with isolated state and process ancestry. Editor
changes also run `npm ci`, coverage tests, build, and asset verification; new
remote refs or unavailable remote commits conservatively run these checks too.
Rebuilt assets must match the committed files. Native development dependencies
are required; editor checks also need Node.js 22 and npm, as in CI.
Linux tests use a systemd user service to avoid inheriting a coding agent's
process ancestry; an unavailable user service manager blocks the push.
Each run owns a unique service. Interrupts and timeouts stop all its processes,
including children that ignore SIGTERM. Cargo/Rust toolchain, target,
profile, compiler and pkg-config build settings are forwarded explicitly;
agent context and registry tokens are not forwarded from the calling shell.
Use `python3 scripts/test-agent-unit-lifecycle.py` on Linux with a working user
service manager to check real cancellation and build-environment forwarding.

The hook does not run license checks, GUI/SSH integration, sanitizers, or another
platform's build. Run `scripts/test-ci.sh linux` or `scripts/test-ci.sh macos`
for the full local gate, and require CI before merging to `main`. Local hooks
can be bypassed and do not replace branch protection.

When `Cargo.lock` or dependency license policy changes, install `cargo-about 0.9.2`
and refresh the checked-in inventory with
`scripts/generate-third-party-licenses.py`. Editor frontend changes use the
workflow documented in the [setup guide](../docs/setup.md#build-from-source).

License policies live in `packaging/licenses/`;
the generated distribution notice and asset/editor credits live in `docs/legal/`.
Run the dependency policy check from the repository root:

```sh
cargo deny --locked --config packaging/licenses/deny.toml check licenses sources
```
