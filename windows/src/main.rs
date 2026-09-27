// SPDX-License-Identifier: GPL-3.0-or-later
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    std::process::exit(flowmux_windows::native::entry::gui_main());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This host targets Windows. Use the root workspace on Linux or macOS.");
    std::process::exit(1);
}
