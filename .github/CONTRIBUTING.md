<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Contributing

Build and test with the commands in the [setup guide](../docs/setup.md#build-from-source).
Keep changes focused, run
`cargo fmt --all`, Clippy with warnings denied, and the locked workspace test
suite before committing. Contributions are licensed under GPL-3.0-or-later.

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
