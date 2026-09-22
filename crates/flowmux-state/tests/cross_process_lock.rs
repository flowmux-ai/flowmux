// SPDX-License-Identifier: GPL-3.0-or-later
//! Cross-process check for [`flowmux_state::try_acquire_state_lock`].
//!
//! Atomic read/merge/write between flowmux windows depends on `flock(2)`
//! working across two different processes. The `instance_lock` unit tests
//! only cover same-process contention, so spawn the
//! `instance_lock_helper` example twice against an isolated
//! `XDG_STATE_HOME` and confirm exactly one process owns the lock.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn helper_path() -> PathBuf {
    // Cargo 1.100 puts test executables below debug/build/<crate>/<hash>/out;
    // older versions use debug/deps. Examples remain below debug/examples.
    let test_exe = std::env::current_exe().expect("current_exe");
    test_exe
        .ancestors()
        .map(|dir| dir.join("examples/instance_lock_helper"))
        .find(|path| path.is_file())
        .expect("examples binary missing — run cargo test -p flowmux-state")
}

#[test]
fn second_process_observes_lock_held_by_first() {
    let helper = helper_path();
    let dir = tempfile::tempdir().unwrap();

    let mut holder = Command::new(&helper)
        .arg("hold")
        .env("XDG_STATE_HOME", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn holder");

    let mut reader = BufReader::new(holder.stdout.take().unwrap());
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "owner", "holder must acquire the lock");

    let probe = Command::new(&helper)
        .arg("probe")
        .env("XDG_STATE_HOME", dir.path())
        .output()
        .expect("spawn probe");
    assert!(probe.status.success(), "{probe:?}");
    assert_eq!(
        String::from_utf8_lossy(&probe.stdout).trim(),
        "none",
        "second process must not get the lock while the first is alive"
    );

    // Release the holder so the lock is dropped, then re-probe.
    let mut stdin = holder.stdin.take().unwrap();
    stdin.write_all(b"x").ok();
    drop(stdin);
    assert!(holder.wait().expect("wait for holder").success());

    let after = Command::new(&helper)
        .arg("probe")
        .env("XDG_STATE_HOME", dir.path())
        .output()
        .expect("re-spawn probe");
    assert!(after.status.success(), "{after:?}");
    assert_eq!(
        String::from_utf8_lossy(&after.stdout).trim(),
        "owner",
        "lock must be re-acquirable once the original holder exits"
    );
}
