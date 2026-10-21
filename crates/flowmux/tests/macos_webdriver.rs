// SPDX-License-Identifier: GPL-3.0-or-later
#[cfg(target_os = "macos")]
#[allow(dead_code)]
#[path = "../src/macos_webdriver/native.rs"]
mod native;

fn main() {
    #[cfg(target_os = "macos")]
    native::smoke();
}
