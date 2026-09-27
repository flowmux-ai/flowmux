// SPDX-License-Identifier: GPL-3.0-or-later
// Installed as flowmux.com so Windows shells select a console-subsystem entry
// point while shortcuts and explicit flowmux.exe remain GUI applications.
#[cfg(windows)]
fn main() {
    std::process::exit(flowmux_windows::native::entry::console_main());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This launcher requires Windows.");
    std::process::exit(1);
}
