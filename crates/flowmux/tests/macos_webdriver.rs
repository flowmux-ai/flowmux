// SPDX-License-Identifier: GPL-3.0-or-later
#[cfg(target_os = "macos")]
#[allow(dead_code)]
#[path = "../src/macos_webdriver/native.rs"]
mod native;

#[cfg(target_os = "macos")]
#[allow(dead_code, unused_imports)]
#[path = "../src/macos_input.rs"]
mod macos_input;

fn main() {
    #[cfg(target_os = "macos")]
    native::smoke();
}
