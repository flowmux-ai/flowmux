// SPDX-License-Identifier: GPL-3.0-or-later
//! Helper binary used by `cross_process_lock.rs` to exercise
//! [`flowmux_state::try_acquire_state_lock`] from a separate process.
//!
//! Two modes, picked by argv\[1\]:
//!   * `hold` — acquire (or fail to acquire) the lock, print
//!     `owner` / `none`, then block on stdin so the caller can keep
//!     the lock held while it spawns a contender.
//!   * `probe` — same lock attempt, print result, exit.

use std::io::Read;

fn main() {
    let mode = std::env::args().nth(1).expect("mode argument required");
    let lock = flowmux_state::try_acquire_state_lock().expect("lock call must not error");
    let label = if lock.is_some() { "owner" } else { "none" };
    println!("{label}");

    match mode.as_str() {
        "hold" => {
            // Hold the lock until the parent sends a byte or closes stdin.
            let mut buf = [0u8; 1];
            let _ = std::io::stdin().read(&mut buf);
        }
        "probe" => {}
        other => panic!("unknown mode: {other}"),
    }
    drop(lock);
}
