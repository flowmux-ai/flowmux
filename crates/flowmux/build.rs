// SPDX-License-Identifier: GPL-3.0-or-later
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        // GTK's own PLT calls must reach the compatibility function, including in tests.
        println!("cargo:rustc-link-arg=-Wl,--export-dynamic-symbol=g_task_run_in_thread");
    }
}
