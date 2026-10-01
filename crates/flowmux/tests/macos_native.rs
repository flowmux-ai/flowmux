// SPDX-License-Identifier: GPL-3.0-or-later
//! Native GTK/WKWebView smoke checks must run on macOS's main thread.
#![allow(
    dead_code,
    unused_imports,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

#[path = "../src/activity.rs"]
mod activity;
#[path = "../src/bridge/mod.rs"]
mod bridge;
#[path = "../src/builtin_icons.rs"]
mod builtin_icons;
#[path = "../src/ipc_handler.rs"]
mod ipc_handler;
#[path = "../src/keybindings.rs"]
mod keybindings;
#[path = "../src/notifications.rs"]
mod notifications;
#[path = "../src/platform.rs"]
mod platform;
#[path = "../src/theme.rs"]
mod theme;
#[path = "../src/ui/mod.rs"]
mod ui;
#[path = "../src/update/mod.rs"]
mod update;
#[path = "../src/usage/mod.rs"]
mod usage;

const APP_ID: &str = "com.flowmux.App";

fn main() {
    #[cfg(target_os = "macos")]
    ui::window::macos_smoke::run();
    #[cfg(not(target_os = "macos"))]
    eprintln!("macos_native requires macOS; native smoke is not run on this platform");
}
