// SPDX-License-Identifier: GPL-3.0-or-later
use flowmux_daemon::StateStore;
use flowmux_state::{State, WindowOwner};
use std::future::Future;
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};

#[test]
fn queued_save_cannot_overwrite_a_newer_final_save() {
    // This integration-test binary owns its environment and state directory.
    let dir = tempfile::tempdir().unwrap();
    std::env::set_var("XDG_STATE_HOME", dir.path());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let blocker = runtime.spawn_blocking(move || {
        entered_tx.send(()).unwrap();
        release_rx.recv().unwrap();
    });
    entered_rx.recv().unwrap();

    let owner = WindowOwner::current();
    let store = StateStore::new_lazy_window(State::default(), owner);
    store.set_sidebar_position_blocking(111);
    let mut queued_save = Box::pin(store.save_now());
    {
        let _entered = runtime.enter();
        assert!(matches!(
            queued_save
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
    }

    store.set_sidebar_position_blocking(222);
    store.save_now_blocking().unwrap();
    let saved_position = || {
        flowmux_state::load()
            .unwrap()
            .windows
            .into_iter()
            .find(|window| window.instance_id == owner.instance_id)
            .unwrap()
            .sidebar_position
    };
    assert_eq!(saved_position(), Some(222));

    release_tx.send(()).unwrap();
    runtime.block_on(queued_save).unwrap();
    runtime.block_on(blocker).unwrap();
    assert_eq!(saved_position(), Some(222));
}
