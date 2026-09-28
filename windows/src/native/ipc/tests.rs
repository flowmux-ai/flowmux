// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::os::windows::fs::OpenOptionsExt;

fn test_name() -> String {
    format!(
        r"\\.\pipe\flowmux-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    )
}

#[test]
fn accept_recovers_when_client_disconnects_before_connect_named_pipe() {
    let name = test_name();
    let mut instance =
        transport::Pipe::new(make_pipe(&name, &user_descriptor().unwrap(), true).unwrap()).unwrap();
    // A successful CreateFile need not wait for the server's ConnectNamedPipe.
    drop(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&name)
            .unwrap(),
    );
    assert_eq!(
        instance
            .connect_once(&transport::Event::new().unwrap())
            .unwrap_err()
            .raw_os_error(),
        Some(ERROR_NO_DATA as i32)
    );
    // Exercise the same acceptance function used by the product listener.
    let (send, receive) = mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = instance.accept(&transport::Event::new().unwrap());
        send.send(result.map_err(|error| error.to_string()))
            .unwrap();
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut client = None;
    while std::time::Instant::now() < deadline {
        if let Ok(file) = OpenOptions::new().read(true).write(true).open(&name) {
            client = Some(file);
            break;
        }
        if worker.is_finished() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let result = receive.recv_timeout(Duration::from_secs(1)).unwrap();
    worker.join().unwrap();
    assert!(
        result.is_ok(),
        "listener stopped after ERROR_NO_DATA: {result:?}"
    );
    assert!(client.is_some(), "the replacement client could not connect");
}

#[test]
fn accept_handles_client_already_connected_and_validates_owner() {
    let name = test_name();
    let mut instance =
        transport::Pipe::new(make_pipe(&name, &user_descriptor().unwrap(), true).unwrap()).unwrap();
    let _client = open_verified_pipe(&name).unwrap();
    instance.accept(&transport::Event::new().unwrap()).unwrap();
}

#[test]
fn pipe_with_wrong_server_pid_is_rejected_before_sending_bytes() {
    let name = format!(
        r"\\.\pipe\flowmux-{}-{}",
        std::process::id() + 1,
        uuid::Uuid::new_v4()
    );
    let mut instance =
        transport::Pipe::new(make_pipe(&name, &user_descriptor().unwrap(), true).unwrap()).unwrap();
    let error = open_verified_pipe(&name).unwrap_err().to_string();
    assert!(error.contains("pipe server PID mismatch"), "{error}");
    // The rejected client is closed without sending a request.
    assert_eq!(
        instance
            .connect_once(&transport::Event::new().unwrap())
            .unwrap_err()
            .raw_os_error(),
        Some(ERROR_NO_DATA as i32)
    );
}

#[test]
fn only_canonical_local_pipe_names_are_accepted() {
    let name = test_name();
    assert_eq!(discovery::pipe_pid(&name).unwrap(), std::process::id());
    for invalid in [
        name.replace(r"\\.\", r"\\remote\"),
        format!("{name}\\other"),
        format!("{name}\0"),
        format!(r"\\.\pipe\flowmux-0-{}", uuid::Uuid::new_v4()),
        format!(r"\\.\pipe\flowmux-01-{}", uuid::Uuid::new_v4()),
        r"\\.\pipe\flowmux-123-../other".into(),
        r"\\.\pipe\flowmux-123-bad".into(),
    ] {
        assert!(discovery::pipe_pid(&invalid).is_err(), "{invalid:?}");
    }
}

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        let directory =
            Self(std::env::temp_dir().join(format!("flowmux-ipc-test-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir_all(&directory.0).unwrap();
        directory
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn discovery_skips_partial_oversized_mismatched_and_temporary_records() {
    let directory = TestDirectory::new();
    let name = test_name();
    let path = discovery::publish(&directory.0, &name).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    std::fs::write(directory.0.join("6.json"), b"{\"pid\":6,").unwrap();
    let oversized_name = format!(r"\\.\pipe\flowmux-5-{}", uuid::Uuid::new_v4());
    let mut oversized = serde_json::to_vec(&json!({"pid":5,"pipe":oversized_name})).unwrap();
    // Valid JSON, canonical name and matching filename: only the byte cap rejects it.
    oversized.resize(4097, b' ');
    std::fs::write(directory.0.join("5.json"), oversized).unwrap();
    std::fs::write(directory.0.join("wrong-filename.json"), &bytes).unwrap();
    std::fs::write(directory.0.join("record.tmp"), &bytes).unwrap();
    std::fs::write(
        directory.0.join("1.json"),
        br#"{"pid":1,"pipe":"\\\\remote\\pipe\\flowmux-1-bad"}"#,
    )
    .unwrap();
    let other = format!(r"\\.\pipe\flowmux-2-{}", uuid::Uuid::new_v4());
    std::fs::write(
        directory.0.join("3.json"),
        serde_json::to_vec(&json!({"pid":3,"pipe":other})).unwrap(),
    )
    .unwrap();
    std::fs::create_dir(directory.0.join("4.json")).unwrap();
    assert_eq!(discovery::candidates(&directory.0), vec![name]);
}

#[test]
fn discovery_publishes_and_replaces_beyond_max_path_with_unicode() {
    use std::os::windows::ffi::OsStrExt;
    let root = TestDirectory::new();
    let mut directory = root.0.clone();
    while directory.as_os_str().encode_wide().count() < 300 {
        directory.push("한글-한-e\u{301}-😀-discovery");
    }
    let old = test_name();
    let new = test_name();
    let path = discovery::publish(&directory, &old).unwrap();
    assert_eq!(discovery::candidates(&directory), vec![old.clone()]);
    discovery::publish(&directory, &new).unwrap();
    discovery::remove_if_current(&path, &old);
    assert_eq!(discovery::candidates(&directory), vec![new.clone()]);
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
    discovery::remove_if_current(&path, &new);
    assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 0);
}

#[test]
fn discovery_atomic_replacement_never_exposes_partial_json() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let directory = TestDirectory::new();
    let names: Vec<_> = (0..40).map(|_| test_name()).collect();
    let path = discovery::publish(&directory.0, &names[0]).unwrap();
    let reading = Arc::new(AtomicBool::new(true));
    let observations = Arc::new(AtomicUsize::new(0));
    let reader = {
        let reading = reading.clone();
        let observations = observations.clone();
        let names = names.clone();
        std::thread::spawn(move || {
            while reading.load(Ordering::Acquire) {
                let value: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                assert!(names.iter().any(|name| value["pipe"] == *name));
                assert_eq!(value["pid"], std::process::id());
                observations.fetch_add(1, Ordering::Release);
            }
        })
    };
    while observations.load(Ordering::Acquire) == 0 && !reader.is_finished() {
        std::thread::yield_now();
    }
    let mut publication = Ok(PathBuf::new());
    for name in &names[1..] {
        publication = discovery::publish(&directory.0, name);
        if publication.is_err() {
            break;
        }
    }
    reading.store(false, Ordering::Release);
    reader.join().unwrap();
    publication.unwrap();
    assert_eq!(
        discovery::candidates(&directory.0),
        vec![names.last().unwrap().clone()]
    );
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn failed_discovery_replacement_preserves_previous_record_and_cleans_temporary_file() {
    let directory = TestDirectory::new();
    let old = test_name();
    let new = test_name();
    let path = discovery::publish(&directory.0, &old).unwrap();
    let previous = std::fs::read(&path).unwrap();
    let lease = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .unwrap();
    assert!(discovery::publish(&directory.0, &new).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), previous);
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 1);
    drop(lease);
    discovery::publish(&directory.0, &new).unwrap();
    discovery::remove_if_current(&path, &old);
    assert!(path.exists());
    discovery::remove_if_current(&path, &new);
    assert!(!path.exists());
}
