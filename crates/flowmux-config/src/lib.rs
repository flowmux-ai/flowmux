// SPDX-License-Identifier: GPL-3.0-or-later
//! User options, keybindings, themes, project commands, paths, and diagnostics.

pub mod cmux_json;
pub mod diagnostics;
pub mod ghostty;
pub mod keybindings;
pub mod options;
pub mod paths;
pub mod presets;
pub mod theme;

/// Shared test helpers. `env_lock` serializes any test that mutates
/// `XDG_*` environment variables — std::env::set_var is racy across
/// threads, and cargo runs tests inside one process by default. Every
/// test module that calls `set_var("XDG_CONFIG_HOME", …)` must hold
/// this lock for the entire bracket of its env mutation.
#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::{Mutex, OnceLock};

    pub fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
}
