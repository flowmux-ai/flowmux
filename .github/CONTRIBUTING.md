<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Contributing

Build and test with the commands in the [setup guide](../docs/setup.md#build-from-source).
Keep changes focused, run
`cargo fmt --all`, Clippy with warnings denied, and the locked workspace test
suite before committing. Contributions are licensed under GPL-3.0-or-later.

Run the headless workspace tests with isolated state and process ancestry:

```sh
python3 scripts/test-agent-unit.py --all
```

Linux tests use a systemd user service to avoid inheriting a coding agent's
process ancestry; an unavailable user service manager fails the test run.
Each run owns a unique service. Interrupts and timeouts stop all its processes,
including children that ignore SIGTERM. Cargo/Rust toolchain, target,
profile, compiler and pkg-config build settings are forwarded explicitly;
agent context and registry tokens are not forwarded from the calling shell.
Use `python3 scripts/test-agent-unit-lifecycle.py` on Linux with a working user
service manager to check real cancellation and build-environment forwarding.

Run `scripts/test-ci.sh linux` or `scripts/test-ci.sh macos` for the full local
gate, and require CI before merging to `main`.
When launching the full Linux gate from a coding agent, run the gate itself in
a separate systemd user service as well: clearing environment variables alone
does not remove agent ancestors from the process table. Use the installed
`flowmux-webkit` AppArmor profile for WebKit user namespaces; do not disable its
sandbox. Give that service a runtime limit and stop the owned service if the
gate is interrupted.

When `Cargo.lock` or dependency license policy changes, install `cargo-about 0.9.2`
and refresh the checked-in inventory with
`scripts/generate-third-party-licenses.py`. Editor frontend changes use the
workflow documented in the [setup guide](../docs/setup.md#build-from-source).

License policies live in `packaging/licenses/`;
the generated distribution notice and asset/editor credits live in `docs/legal/`.
Run the dependency policy check from the repository root:

```sh
cargo deny --locked check --config packaging/licenses/deny.toml advisories licenses sources
```
