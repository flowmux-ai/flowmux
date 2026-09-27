// SPDX-License-Identifier: GPL-3.0-or-later
#[cfg(windows)]
fn main() {
    use clap::Parser;
    if let Err(error) = flowmux_windows::native::ipc::run(flowmux_windows::command::Cli::parse()) {
        eprintln!("flowmuxctl: {error:#}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    use clap::Parser;
    let _ = flowmux_windows::command::Cli::parse();
    eprintln!("This IPC client requires Windows.");
    std::process::exit(1);
}
