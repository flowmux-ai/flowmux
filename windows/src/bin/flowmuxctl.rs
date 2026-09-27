// SPDX-License-Identifier: GPL-3.0-or-later
#[cfg(windows)]
fn main() {
    std::process::exit(flowmux_windows::native::entry::control_main());
}

#[cfg(not(windows))]
fn main() {
    use clap::Parser;
    let _ = flowmux_windows::command::Cli::parse();
    eprintln!("This IPC client requires Windows.");
    std::process::exit(1);
}
