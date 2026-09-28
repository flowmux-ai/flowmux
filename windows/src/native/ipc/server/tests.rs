// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

fn reply_inbox() -> (Reply, ReplyInbox, Arc<AtomicUsize>) {
    let pending = Arc::new(AtomicUsize::new(0));
    let (send, receive) = mpsc::sync_channel(1);
    let reply = Reply::new(send, &pending).unwrap();
    let inbox = ReplyInbox::new(receive, &reply);
    (reply, inbox, pending)
}

#[test]
fn reply_sent_after_poll_timeout_but_before_retirement_is_consumed_once() {
    let (reply, inbox, pending) = reply_inbox();
    assert_eq!(
        inbox.receiver.recv_timeout(Duration::ZERO),
        Err(mpsc::RecvTimeoutError::Timeout)
    );
    let clone = reply.clone();
    assert_eq!(clone.received_at(), reply.received_at());
    let (release, gate) = mpsc::sync_channel(1);
    let (sent, observed) = mpsc::sync_channel(1);
    let sender = std::thread::spawn(move || {
        gate.recv_timeout(Duration::from_secs(2)).unwrap();
        sent.send(clone.try_send(json!({"ok":true}))).unwrap();
    });
    // The previous receive timed out, then the UI wins the shared delivery gate
    // before the server retires the request. No wall-clock race is required.
    release.send(()).unwrap();
    assert!(observed
        .recv_timeout(Duration::from_secs(2))
        .unwrap()
        .is_ok());
    assert_eq!(inbox.close_and_drain().unwrap(), json!({"ok":true}));
    assert!(matches!(
        reply.try_send(json!({"duplicate":true})),
        Err(mpsc::TrySendError::Disconnected(_))
    ));
    assert_eq!(inbox.receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
    sender.join().unwrap();
    drop(inbox);
    assert_eq!(pending.load(Ordering::Acquire), 1);
    drop(reply);
    assert_eq!(pending.load(Ordering::Acquire), 0);
}

#[test]
fn deadline_retirement_before_send_disconnects_all_clones_without_refunding_the_lease() {
    let (reply, inbox, pending) = reply_inbox();
    let clone = reply.clone();
    let retained = reply.clone();
    let (release, gate) = mpsc::sync_channel(1);
    let (sent, observed) = mpsc::sync_channel(1);
    let sender = std::thread::spawn(move || {
        gate.recv_timeout(Duration::from_secs(2)).unwrap();
        sent.send(clone.try_send(json!({"ok":true}))).unwrap();
    });
    assert_eq!(inbox.close_and_drain(), Err(mpsc::TryRecvError::Empty));
    release.send(()).unwrap();
    assert!(matches!(
        observed.recv_timeout(Duration::from_secs(2)).unwrap(),
        Err(mpsc::TrySendError::Disconnected(value)) if value == json!({"ok":true})
    ));
    // The Receiver is deliberately still alive: the closed delivery gate, not
    // dropping the channel, makes a late accepted-close sender take fallback.
    assert_eq!(inbox.receiver.try_recv(), Err(mpsc::TryRecvError::Empty));
    assert!(matches!(
        retained.try_send(json!({"another":true})),
        Err(mpsc::TrySendError::Disconnected(_))
    ));
    sender.join().unwrap();
    drop(reply);
    drop(inbox);
    assert_eq!(pending.load(Ordering::Acquire), 1);
    drop(retained);
    assert_eq!(pending.load(Ordering::Acquire), 0);
}

#[test]
fn receiver_gate_does_not_retain_a_sender_or_command_lease() {
    let (reply, inbox, pending) = reply_inbox();
    drop(reply);
    assert_eq!(pending.load(Ordering::Acquire), 0);
    assert_eq!(
        inbox.receiver.recv_timeout(Duration::ZERO),
        Err(mpsc::RecvTimeoutError::Disconnected)
    );
    assert_eq!(
        inbox.close_and_drain(),
        Err(mpsc::TryRecvError::Disconnected)
    );
}

#[test]
fn dropping_inbox_closes_delivery_but_keeps_live_ui_reply_leases() {
    let (reply, inbox, pending) = reply_inbox();
    let retained = reply.clone();
    drop(inbox);
    assert_eq!(pending.load(Ordering::Acquire), 1);
    assert!(matches!(
        reply.try_send(json!({"ok":true})),
        Err(mpsc::TrySendError::Disconnected(_))
    ));
    drop(reply);
    assert_eq!(pending.load(Ordering::Acquire), 1);
    drop(retained);
    assert_eq!(pending.load(Ordering::Acquire), 0);
}

#[test]
fn poisoned_delivery_gate_still_retires_without_losing_a_queued_reply() {
    let (reply, inbox, pending) = reply_inbox();
    let gate = reply.accepting.clone();
    assert!(std::panic::catch_unwind(move || {
        let _guard = gate.lock().unwrap();
        panic!("deliberate delivery-gate poison");
    })
    .is_err());
    reply.try_send(json!({"ok":true})).unwrap();
    assert_eq!(inbox.close_and_drain().unwrap(), json!({"ok":true}));
    assert!(matches!(
        reply.try_send(json!({"late":true})),
        Err(mpsc::TrySendError::Disconnected(_))
    ));
    drop(inbox);
    drop(reply);
    assert_eq!(pending.load(Ordering::Acquire), 0);
}

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("flowmux-ipc-pool-{}", uuid::Uuid::new_v4())))
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn short_limits() -> Limits {
    Limits {
        request: Duration::from_millis(500),
        command: Duration::from_millis(150),
        write: Duration::from_millis(100),
        close: Duration::from_millis(100),
    }
}
fn request(pipe: &mut Pipe, bytes: &[u8]) -> Value {
    let stop = Event::new().unwrap();
    pipe.write_all(bytes, Instant::now() + Duration::from_secs(2), &stop)
        .unwrap();
    serde_json::from_slice(
        &pipe
            .read_frame(
                MAX_REPLY_BYTES,
                Instant::now() + Duration::from_secs(2),
                &stop,
            )
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn fixed_pool_rejects_excess_connections_and_reuses_expired_slots() {
    let directory = Directory::new();
    let server = Server::start_in(
        &directory.0,
        |_, reply| {
            let _ = reply.try_send(json!({"ok":true}));
        },
        || {},
        short_limits(),
    )
    .unwrap();
    assert_eq!(server.workers.len(), MAX_CLIENTS as usize);
    let clients: Vec<_> = (0..MAX_CLIENTS)
        .map(|_| open_verified_pipe(&server.name).unwrap())
        .collect();
    let error = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&server.name)
        .unwrap_err();
    assert_eq!(error.raw_os_error(), Some(ERROR_PIPE_BUSY as i32));
    let mut replacement = open_verified_pipe(&server.name).unwrap();
    assert_eq!(
        request(&mut replacement, b"{\"method\":\"identify\"}\n"),
        json!({"ok":true})
    );
    drop(replacement);
    assert_eq!(clients.len(), MAX_CLIENTS as usize); // Old handles remain open but no longer occupy slots.
    drop(server);
}

#[test]
fn stalled_command_and_unread_large_reply_release_capacity() {
    let directory = Directory::new();
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let dispatches = Arc::new(AtomicUsize::new(0));
    let copy = held.clone();
    let count = dispatches.clone();
    let server = Server::start_in(
        &directory.0,
        move |request, reply| {
            count.fetch_add(1, Ordering::SeqCst);
            if matches!(request.command, Command::Tree) {
                copy.lock().unwrap().push(reply);
            } else {
                let _ = reply.try_send(json!({"text":"한".repeat(100_000)}));
            }
        },
        || {},
        short_limits(),
    )
    .unwrap();
    let mut client = open_verified_pipe(&server.name).unwrap();
    let result = request(&mut client, b"{\"method\":\"tree\"}\n");
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("command deadline"));
    drop(client);
    held.lock().unwrap().clear(); // The UI releases the expired command's permit.
    let stop = Event::new().unwrap();
    let mut clients: Vec<_> = (0..MAX_CLIENTS)
        .map(|_| open_verified_pipe(&server.name).unwrap())
        .collect();
    for pipe in &mut clients {
        pipe.write_all(
            b"{\"method\":\"identify\"}\n",
            Instant::now() + Duration::from_secs(1),
            &stop,
        )
        .unwrap();
    }
    std::thread::sleep(Duration::from_millis(400));
    let mut next = open_verified_pipe(&server.name).unwrap();
    let result = request(&mut next, b"{\"method\":\"tree\"}\n");
    assert!(result.get("error").is_some());
    assert_eq!(dispatches.load(Ordering::SeqCst), MAX_CLIENTS as usize + 2);
    drop(next);
    drop(server);
}

#[test]
fn server_drop_cancels_accept_read_write_and_command_waits_without_gui_progress() {
    let directory = Directory::new();
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let copy = held.clone();
    let server = Server::start_in(
        &directory.0,
        move |request, reply| {
            if matches!(request.command, Command::Identify) {
                let _ = reply.try_send(json!({"text":"x".repeat(300_000)}));
            } else {
                copy.lock().unwrap().push(reply);
            }
        },
        || {},
        Limits::default(),
    )
    .unwrap();
    let path = server.discovery.clone();
    let name = server.name.clone();
    let mut waiting_reply = open_verified_pipe(&name).unwrap();
    waiting_reply
        .write_all(
            b"{\"method\":\"tree\"}\n",
            Instant::now() + Duration::from_secs(1),
            &Event::new().unwrap(),
        )
        .unwrap();
    let _waiting_request = open_verified_pipe(&name).unwrap();
    let mut unread_reply = open_verified_pipe(&name).unwrap();
    unread_reply
        .write_all(
            b"{\"method\":\"identify\"}\n",
            Instant::now() + Duration::from_secs(1),
            &Event::new().unwrap(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while held.lock().unwrap().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(held.lock().unwrap().len(), 1);
    std::thread::sleep(Duration::from_millis(20));
    let start = Instant::now();
    drop(server);
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(!path.exists());
    assert!(open_verified_pipe(&name).is_err());
}

#[test]
fn timed_out_commands_cannot_fill_the_ui_queue_beyond_the_permit_limit() {
    let directory = Directory::new();
    let held = Arc::new(std::sync::Mutex::new(Vec::new()));
    let copy = held.clone();
    let mut limits = short_limits();
    limits.command = Duration::from_millis(15);
    let server = Server::start_in(
        &directory.0,
        move |_, reply| {
            copy.lock().unwrap().push(reply);
        },
        || {},
        limits,
    )
    .unwrap();
    for _ in 0..MAX_CLIENTS {
        let mut client = open_verified_pipe(&server.name).unwrap();
        assert!(request(&mut client, b"{\"method\":\"tree\"}\n")["error"]
            .as_str()
            .unwrap()
            .contains("command deadline"));
    }
    let mut client = open_verified_pipe(&server.name).unwrap();
    assert!(request(&mut client, b"{\"method\":\"tree\"}\n")["error"]
        .as_str()
        .unwrap()
        .contains("was not dispatched"));
    drop(client);
    assert_eq!(held.lock().unwrap().len(), MAX_CLIENTS as usize);
    let retained = held.lock().unwrap()[0].clone();
    held.lock().unwrap().clear();
    assert_eq!(retained._lease.0.load(Ordering::SeqCst), 1);
    let mut client = open_verified_pipe(&server.name).unwrap();
    assert!(request(&mut client, b"{\"method\":\"tree\"}\n")["error"]
        .as_str()
        .unwrap()
        .contains("command deadline"));
    assert_eq!(held.lock().unwrap().len(), 1);
    drop(client);
    drop(server);
}

#[test]
fn repeated_pool_shutdown_releases_pipe_event_and_thread_handles() {
    fn handles() -> u32 {
        let mut count = 0;
        unsafe {
            checked(GetProcessHandleCount(GetCurrentProcess(), &mut count)).unwrap();
        }
        count
    }
    let directory = Directory::new();
    let before = handles();
    for _ in 0..10 {
        let server = Server::start_in(
            &directory.0,
            |_, reply| {
                let _ = reply.try_send(json!({"ok":true}));
            },
            || {},
            short_limits(),
        )
        .unwrap();
        let mut client = open_verified_pipe(&server.name).unwrap();
        assert_eq!(
            request(&mut client, b"{\"method\":\"identify\"}\n"),
            json!({"ok":true})
        );
        drop(client);
        drop(server);
    }
    let after = handles();
    assert!(
        after <= before + 2,
        "handle count grew across ten pool lifetimes: {before} -> {after}"
    );
}

#[test]
fn unicode_fragmented_request_has_one_dispatch_and_large_response_is_complete() {
    let directory = Directory::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let copy = calls.clone();
    let expected = "한글 한 e\u{301} 😀".repeat(20_000);
    let response = expected.clone();
    let server = Server::start_in(
        &directory.0,
        move |_, reply| {
            copy.fetch_add(1, Ordering::SeqCst);
            let _ = reply.try_send(json!({"text":response}));
        },
        || {},
        Limits::default(),
    )
    .unwrap();
    let mut client = open_verified_pipe(&server.name).unwrap();
    let stop = Event::new().unwrap();
    let mut bytes = serde_json::to_vec(&Request {
        command: Command::Identify,
        caller_surface: None,
        caller_cwd: Some(PathBuf::from("C:\\한글 한 😀")),
    })
    .unwrap();
    bytes.push(b'\n');
    for chunk in bytes.chunks(2) {
        client
            .write_all(chunk, Instant::now() + Duration::from_secs(1), &stop)
            .unwrap();
    }
    let value: Value = serde_json::from_slice(
        &client
            .read_frame(
                MAX_REPLY_BYTES,
                Instant::now() + Duration::from_secs(5),
                &stop,
            )
            .unwrap(),
    )
    .unwrap();
    assert_eq!(value["text"], expected);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    drop(client);
    drop(server);
}

#[test]
fn frame_timeout_cancels_pending_read_and_write_without_reusing_buffers_early() {
    let name = format!(
        r"\\.\pipe\flowmux-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    );
    let mut server =
        Pipe::new(make_pipe(&name, &user_descriptor().unwrap(), true).unwrap()).unwrap();
    let mut client = open_verified_pipe(&name).unwrap();
    let stop = Event::new().unwrap();
    server.accept(&stop).unwrap();
    let start = Instant::now();
    assert_eq!(
        client
            .read_frame(1024, start + Duration::from_millis(80), &stop)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::TimedOut
    );
    let bytes = vec![b'x'; 1024 * 1024];
    assert_eq!(
        client
            .write_all(&bytes, Instant::now() + Duration::from_millis(80), &stop)
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::TimedOut
    );
    assert!(start.elapsed() < Duration::from_secs(1));
    server.disconnect().unwrap();
    drop(client);
    // The same server instance is still usable after the peer cancels its I/O.
    let worker = std::thread::spawn(move || {
        let stop = Event::new().unwrap();
        server.accept(&stop).unwrap();
        server
            .read_frame(10, Instant::now() + Duration::from_secs(1), &stop)
            .unwrap()
    });
    let mut client = open_verified_pipe(&name).unwrap();
    client
        .write_all(b"new\n", Instant::now() + Duration::from_secs(1), &stop)
        .unwrap();
    assert_eq!(worker.join().unwrap(), b"new\n");
}

#[test]
fn oversized_and_unterminated_frames_are_rejected_without_dispatch() {
    let directory = Directory::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let copy = calls.clone();
    let server = Server::start_in(
        &directory.0,
        move |_, reply| {
            copy.fetch_add(1, Ordering::SeqCst);
            let _ = reply.try_send(json!({"ok":true}));
        },
        || {},
        short_limits(),
    )
    .unwrap();
    let mut client = open_verified_pipe(&server.name).unwrap();
    let stop = Event::new().unwrap();
    client
        .write_all(
            &vec![b'x'; MAX_MESSAGE_BYTES],
            Instant::now() + Duration::from_secs(2),
            &stop,
        )
        .unwrap();
    assert!(client
        .read_frame(100, Instant::now() + Duration::from_secs(1), &stop)
        .is_err());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(serde_json::from_slice::<Value>(&encode_reply(
        &json!({"text":"x".repeat(MAX_REPLY_BYTES)})
    ))
    .unwrap()
    .get("error")
    .is_some());
}

#[test]
fn accepted_quit_finishes_even_if_client_keeps_reply_unread() {
    let directory = Directory::new();
    let quits = Arc::new(AtomicUsize::new(0));
    let copy = quits.clone();
    let server = Server::start_in(
        &directory.0,
        |_, reply| {
            let _ = reply.try_send(json!({"ok":true}));
        },
        move || {
            copy.fetch_add(1, Ordering::SeqCst);
        },
        short_limits(),
    )
    .unwrap();
    let mut client = open_verified_pipe(&server.name).unwrap();
    client
        .write_all(
            b"{\"method\":\"quit\",\"discard_state\":true}\n",
            Instant::now() + Duration::from_secs(1),
            &Event::new().unwrap(),
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(2);
    while quits.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(quits.load(Ordering::SeqCst), 1);
    drop(server);
}
