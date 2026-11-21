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

/// Environment-dependent tests share this lock across reads and mutations.
/// Hold it for the entire assertion or mutation bracket, including restoration.
#[cfg(test)]
pub(crate) mod test_env {
    use std::sync::{Mutex, OnceLock};

    pub fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
}
