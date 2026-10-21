// SPDX-License-Identifier: GPL-3.0-or-later
#![allow(dead_code, unused_imports)]
#[cfg(target_os = "macos")]
#[path = "../src/main.rs"]
mod viewer;

fn main() {
    #[cfg(target_os = "macos")]
    viewer::native_smoke();
}
