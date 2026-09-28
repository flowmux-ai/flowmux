// SPDX-License-Identifier: GPL-3.0-or-later
//! Owned-root change hints, without document/model/UI access.
//! A Ready notice requires initial byte reconciliation. Acknowledge a notice only
//! after its corresponding frontend result is applied, including partial errors.
//! An acknowledgment means delivery was handled, not that every file was readable.

use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Changed,
    Ready,
    Rescan,
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct Notice {
    pub epoch: Uuid,
    pub generation: u64,
    pub kind: Kind,
    pub ready: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub epoch: Uuid,
    pub generation: u64,
    pub acknowledged: u64,
    pub in_flight: Option<u64>,
    pub ready: bool,
    pub stopping: bool,
    pub stopped: bool,
    pub cancellation_over_budget: bool,
    pub last_error: Option<String>,
}

#[cfg(any(windows, test))]
struct Coalescer {
    status: Status,
    queued: Option<Kind>,
}
#[cfg(any(windows, test))]
impl Coalescer {
    fn new(epoch: Uuid) -> Self {
        Self {
            status: Status {
                epoch,
                generation: 0,
                acknowledged: 0,
                in_flight: None,
                ready: false,
                stopping: false,
                stopped: false,
                cancellation_over_budget: false,
                last_error: None,
            },
            queued: None,
        }
    }
    fn mark(&mut self, kind: Kind, error: Option<String>) {
        // A u64 cannot exhaust within a practical watcher lifetime. Never wrap a
        // generation into an old acknowledgment if that invariant is violated.
        self.status.generation = self
            .status
            .generation
            .checked_add(1)
            .expect("watch generation exhausted");
        if kind == Kind::Ready {
            self.status.ready = true;
        }
        if kind == Kind::Failed {
            self.status.ready = false;
        }
        if let Some(error) = error {
            self.status.last_error = Some(error.chars().take(4096).collect());
        }
        self.queued = Some(self.queued.map_or(kind, |old| old.max(kind)));
    }
    fn claim(&mut self) -> Option<Notice> {
        if self.status.stopping || self.status.in_flight.is_some() {
            return None;
        }
        let kind = self.queued.take()?;
        self.status.in_flight = Some(self.status.generation);
        Some(Notice {
            epoch: self.status.epoch,
            generation: self.status.generation,
            kind,
            ready: self.status.ready,
            error: self.status.last_error.clone(),
        })
    }
    fn acknowledge(&mut self, epoch: Uuid, generation: u64) -> bool {
        if self.status.stopping
            || epoch != self.status.epoch
            || self.status.in_flight != Some(generation)
        {
            return false;
        }
        self.status.in_flight = None;
        self.status.acknowledged = generation;
        true
    }
}

#[cfg(windows)]
pub use native::Watcher;

#[cfg(windows)]
mod native {
    use super::*;
    use anyhow::{ensure, Context};
    use std::{
        io,
        os::windows::{
            ffi::OsStrExt,
            io::{AsRawHandle, FromRawHandle, OwnedHandle},
        },
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex, MutexGuard,
        },
        thread::{self, JoinHandle},
        time::{Duration, Instant},
    };
    use windows_sys::Win32::{
        Foundation::{
            ERROR_IO_INCOMPLETE, ERROR_IO_PENDING, ERROR_NOTIFY_ENUM_DIR, ERROR_OPERATION_ABORTED,
            INVALID_HANDLE_VALUE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
        },
        Storage::FileSystem::{
            CreateFileW, ReadDirectoryChangesW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED,
            FILE_LIST_DIRECTORY, FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME,
            FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE,
            FILE_NOTIFY_CHANGE_SECURITY, FILE_NOTIFY_CHANGE_SIZE, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        },
        System::{
            Threading::{
                CreateEventW, ResetEvent, SetEvent, WaitForMultipleObjects, WaitForSingleObject,
            },
            IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED},
        },
    };

    const WAIT_SLICE_MS: u32 = 250;
    const CANCEL_GRACE: Duration = Duration::from_secs(2);
    const BUFFER_WORDS: usize = 16 * 1024; // 64 KiB, DWORD-aligned.

    struct Event(OwnedHandle);
    impl Event {
        fn new(manual: bool) -> io::Result<Self> {
            let raw = unsafe {
                CreateEventW(
                    std::ptr::null(),
                    if manual { 1 } else { 0 },
                    0,
                    std::ptr::null(),
                )
            };
            if raw.is_null() {
                return Err(io::Error::last_os_error());
            }
            Ok(Self(unsafe { OwnedHandle::from_raw_handle(raw) }))
        }
        fn signal(&self) {
            unsafe {
                SetEvent(self.0.as_raw_handle());
            }
        }
    }
    struct Shared {
        stop: AtomicBool,
        stop_event: Event,
        acknowledge_event: Event,
        state: Mutex<Coalescer>,
    }
    impl Shared {
        fn state(&self) -> MutexGuard<'_, Coalescer> {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        }
        fn stopping(&self) -> bool {
            self.stop.load(Ordering::Acquire)
        }
        fn request_stop(&self) {
            self.stop.store(true, Ordering::Release);
            {
                let mut state = self.state();
                state.status.stopping = true;
                state.status.ready = false;
            }
            self.stop_event.signal(); // A timed wait also observes the atomic flag.
        }
        fn failed(&self, error: impl std::fmt::Display) {
            self.state().mark(Kind::Failed, Some(error.to_string()));
        }
    }

    pub struct Watcher {
        shared: Arc<Shared>,
        thread: Option<JoinHandle<()>>,
    }
    impl Watcher {
        /// Only pure path validation and event/thread creation happen here.
        /// Root canonicalization and native directory opening run on the worker.
        /// `emit` must be a nonblocking event-channel send. It is never called
        /// while holding state or from acknowledge/stop/the UI thread.
        pub fn start(
            root: PathBuf,
            emit: impl Fn(Notice) + Send + 'static,
        ) -> anyhow::Result<Self> {
            crate::editor::validate_path(&root)?;
            ensure!(
                root.is_absolute(),
                "watch root must be a local absolute path"
            );
            let shared = Arc::new(Shared {
                stop: AtomicBool::new(false),
                stop_event: Event::new(true)?,
                acknowledge_event: Event::new(false)?,
                state: Mutex::new(Coalescer::new(Uuid::new_v4())),
            });
            let worker_shared = shared.clone();
            let thread = thread::Builder::new()
                .name("flowmux-editor-watch".into())
                .spawn(move || run(root, worker_shared, emit))
                .context("cannot start editor watch worker")?;
            Ok(Self {
                shared,
                thread: Some(thread),
            })
        }
        pub fn status(&self) -> Status {
            self.shared.state().status.clone()
        }
        /// Exact old acknowledgments cannot release a newer notice or watcher.
        pub fn acknowledge(&self, epoch: Uuid, generation: u64) -> bool {
            let accepted = self.shared.state().acknowledge(epoch, generation);
            if accepted {
                self.shared.acknowledge_event.signal();
            }
            accepted
        }
        /// Nonblocking. `status().stopped` confirms actual worker teardown later.
        pub fn stop(&self) {
            self.shared.request_stop();
        }
    }
    impl Drop for Watcher {
        fn drop(&mut self) {
            self.stop();
            self.thread.take(); // detach; no filesystem/kernel completion join on UI.
        }
    }

    // The kernel sees only heap-stable OVERLAPPED and buffer allocations. The
    // operation object stays on its worker; no unsafe Send/Sync implementation.
    struct DirectoryRead {
        directory: OwnedHandle,
        done: Event,
        overlapped: Box<OVERLAPPED>,
        buffer: Box<[u32; BUFFER_WORDS]>,
        pending: bool,
        shared: Arc<Shared>,
    }
    impl DirectoryRead {
        fn open(root: PathBuf, shared: Arc<Shared>) -> anyhow::Result<Self> {
            let root = crate::editor::canonical_root(&root)?;
            let text = root.to_str().context("watch root must be Unicode")?;
            // canonical_root rejects UNC/devices/ADS and removes std's verbatim prefix.
            let extended = PathBuf::from(format!("\\\\?\\{text}"));
            let mut wide: Vec<u16> = extended.as_os_str().encode_wide().collect();
            ensure!(
                !wide.contains(&0) && wide.len() < crate::editor::MAX_PATH_UNITS,
                "watch root exceeds the native path limit"
            );
            wide.push(0);
            let raw = unsafe {
                CreateFileW(
                    wide.as_ptr(),
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                )
            };
            if raw == INVALID_HANDLE_VALUE {
                return Err(io::Error::last_os_error()).context("cannot open editor watch root");
            }
            let directory = unsafe { OwnedHandle::from_raw_handle(raw) };
            let done = Event::new(true)?;
            let overlapped = Box::new(OVERLAPPED {
                hEvent: done.0.as_raw_handle(),
                ..Default::default()
            });
            Ok(Self {
                directory,
                done,
                overlapped,
                buffer: Box::new([0; BUFFER_WORDS]),
                pending: false,
                shared,
            })
        }
        fn arm(&mut self) -> io::Result<bool> {
            assert!(!self.pending, "never reuse an outstanding OVERLAPPED");
            if unsafe { ResetEvent(self.done.0.as_raw_handle()) } == 0 {
                return Err(io::Error::last_os_error());
            }
            *self.overlapped = OVERLAPPED {
                hEvent: self.done.0.as_raw_handle(),
                ..Default::default()
            };
            let ok = unsafe {
                ReadDirectoryChangesW(
                    self.directory.as_raw_handle(),
                    self.buffer.as_mut_ptr().cast(),
                    (BUFFER_WORDS * 4) as u32,
                    1,
                    FILE_NOTIFY_CHANGE_FILE_NAME
                        | FILE_NOTIFY_CHANGE_DIR_NAME
                        | FILE_NOTIFY_CHANGE_ATTRIBUTES
                        | FILE_NOTIFY_CHANGE_SIZE
                        | FILE_NOTIFY_CHANGE_LAST_WRITE
                        | FILE_NOTIFY_CHANGE_SECURITY,
                    std::ptr::null_mut(),
                    &mut *self.overlapped,
                    None,
                )
            };
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_NOTIFY_ENUM_DIR as i32) {
                    return Ok(false);
                }
                if error.raw_os_error() != Some(ERROR_IO_PENDING as i32) {
                    return Err(error);
                }
            }
            self.pending = true;
            Ok(true)
        }
        // Some(bytes) is success or overflow; None means the kernel still owns
        // the memory. Every other Win32 completion error also completes ownership.
        fn completed(&mut self) -> io::Result<Option<u32>> {
            let mut bytes = 0;
            if unsafe {
                GetOverlappedResult(
                    self.directory.as_raw_handle(),
                    &*self.overlapped,
                    &mut bytes,
                    0,
                )
            } != 0
            {
                self.pending = false;
                return Ok(Some(bytes));
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_IO_INCOMPLETE as i32) {
                return Ok(None);
            }
            self.pending = false;
            if error.raw_os_error() == Some(ERROR_NOTIFY_ENUM_DIR as i32) {
                return Ok(Some(0));
            }
            Err(error)
        }
        fn cancel_and_drain(&mut self) {
            if !self.pending {
                return;
            }
            unsafe {
                CancelIoEx(self.directory.as_raw_handle(), &*self.overlapped);
            }
            let started = Instant::now();
            loop {
                match self.completed() {
                    Ok(Some(_)) | Err(_) => break, // success/cancellation raced; either is complete.
                    Ok(None) => {}
                }
                if started.elapsed() >= CANCEL_GRACE {
                    self.shared.state().status.cancellation_over_budget = true;
                }
                // Never free pending I/O to pretend a driver met our grace. This
                // detached worker retains all memory/handles until completion.
                // The UI stop/drop path already returned without joining it.
                let wait =
                    unsafe { WaitForSingleObject(self.done.0.as_raw_handle(), WAIT_SLICE_MS) };
                if wait == WAIT_FAILED {
                    // Preserve pending memory even if event waiting itself fails.
                    // GetOverlappedResult will still be retried on this worker.
                    thread::park_timeout(Duration::from_millis(u64::from(WAIT_SLICE_MS)));
                }
            }
        }
    }
    impl Drop for DirectoryRead {
        fn drop(&mut self) {
            self.cancel_and_drain();
        }
    }

    fn publish(shared: &Shared, emit: &impl Fn(Notice)) -> bool {
        let notice = shared.state().claim();
        let Some(notice) = notice else {
            return true;
        };
        // A callback panic must not unwind/free an outstanding kernel buffer.
        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| emit(notice))).is_err() {
            shared.failed("editor watcher callback panicked");
            shared.request_stop();
            return false;
        }
        true
    }
    fn failed_pump(shared: &Shared, emit: &impl Fn(Notice)) {
        // Preserve one-error delivery if an earlier notice was still awaiting ACK.
        // Native may stop/recreate after observing failure; there is no reopen loop.
        while !shared.stopping() {
            if !publish(shared, emit) {
                break;
            }
            let handles = [
                shared.stop_event.0.as_raw_handle(),
                shared.acknowledge_event.0.as_raw_handle(),
            ];
            if unsafe { WaitForMultipleObjects(2, handles.as_ptr(), 0, WAIT_SLICE_MS) }
                == WAIT_FAILED
            {
                thread::park_timeout(Duration::from_millis(u64::from(WAIT_SLICE_MS)));
            }
        }
    }
    fn run(root: PathBuf, shared: Arc<Shared>, emit: impl Fn(Notice)) {
        if shared.stopping() {
            shared.state().status.stopped = true;
            return;
        }
        let result = (|| -> anyhow::Result<()> {
            let mut read = DirectoryRead::open(root, shared.clone())?;
            if shared.stopping() {
                return Ok(());
            }
            let mut armed_once = false;
            loop {
                if shared.stopping() {
                    break;
                }
                if !read.pending {
                    if read.arm()? {
                        if !armed_once {
                            // Initial reconciliation covers pre-registration changes.
                            shared.state().mark(Kind::Ready, None);
                            armed_once = true;
                        }
                    } else {
                        // ERROR_NOTIFY_ENUM_DIR can be returned at issue time,
                        // not only by a later overlapped completion.
                        shared.state().mark(Kind::Rescan, None);
                    }
                }
                if !publish(&shared, &emit) {
                    break;
                }
                if !read.pending {
                    // Bound repeated issue-time overflow attempts without a spin.
                    unsafe {
                        WaitForSingleObject(shared.stop_event.0.as_raw_handle(), WAIT_SLICE_MS);
                    }
                    continue;
                }
                let handles = [
                    shared.stop_event.0.as_raw_handle(),
                    shared.acknowledge_event.0.as_raw_handle(),
                    read.done.0.as_raw_handle(),
                ];
                let wait = unsafe { WaitForMultipleObjects(3, handles.as_ptr(), 0, WAIT_SLICE_MS) };
                if shared.stopping() || wait == WAIT_OBJECT_0 {
                    break;
                }
                if wait == WAIT_OBJECT_0 + 1 || wait == WAIT_TIMEOUT {
                    continue;
                }
                if wait == WAIT_OBJECT_0 + 2 {
                    match read.completed() {
                        Ok(Some(bytes)) => {
                            shared.state().mark(
                                if bytes == 0 {
                                    Kind::Rescan
                                } else {
                                    Kind::Changed
                                },
                                None,
                            );
                            // Loop rearms before callback dispatch. The directory
                            // handle retains notifications between read requests.
                        }
                        Ok(None) => continue,
                        Err(error)
                            if error.raw_os_error() == Some(ERROR_OPERATION_ABORTED as i32)
                                && shared.stopping() =>
                        {
                            break
                        }
                        Err(error) => {
                            return Err(error).context("editor directory notification failed")
                        }
                    }
                } else {
                    return Err(io::Error::last_os_error()).context("editor directory wait failed");
                }
            }
            // DirectoryRead Drop completes cancellation here, on this worker only.
            Ok(())
        })();
        if let Err(error) = result {
            shared.failed(format!("{error:#}"));
            failed_pump(&shared, &emit);
        }
        let mut state = shared.state();
        state.status.ready = false;
        state.status.stopped = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ready_requires_initial_reconciliation_and_exact_ack() {
        let epoch = Uuid::from_u128(1);
        let mut queue = Coalescer::new(epoch);
        queue.mark(Kind::Ready, None);
        let initial = queue.claim().unwrap();
        assert_eq!(initial.kind, Kind::Ready);
        assert!(initial.ready);
        assert!(!queue.acknowledge(Uuid::from_u128(2), initial.generation));
        assert!(!queue.acknowledge(epoch, initial.generation + 1));
        assert!(queue.claim().is_none());
        assert!(queue.acknowledge(epoch, initial.generation));
        assert!(!queue.acknowledge(epoch, initial.generation));
        assert!(queue.claim().is_none());
    }
    #[test]
    fn changes_during_inflight_notice_coalesce_without_losing_overflow() {
        let epoch = Uuid::from_u128(1);
        let mut queue = Coalescer::new(epoch);
        queue.mark(Kind::Ready, None);
        let initial = queue.claim().unwrap();
        for _ in 0..10_000 {
            queue.mark(Kind::Changed, None);
        }
        queue.mark(Kind::Rescan, None);
        assert!(queue.claim().is_none());
        assert!(queue.acknowledge(epoch, initial.generation));
        let later = queue.claim().unwrap();
        assert_eq!(later.kind, Kind::Rescan);
        assert_eq!(later.generation, 10_002);
        assert!(!queue.acknowledge(epoch, initial.generation));
        assert_eq!(queue.status.in_flight, Some(later.generation));
    }
    #[test]
    fn later_failure_survives_outstanding_changed_notice() {
        let epoch = Uuid::from_u128(1);
        let mut queue = Coalescer::new(epoch);
        queue.mark(Kind::Changed, None);
        let first = queue.claim().unwrap();
        queue.mark(
            Kind::Failed,
            Some("root 한글 한 é 😀 is unavailable".into()),
        );
        assert!(queue.claim().is_none());
        queue.acknowledge(epoch, first.generation);
        let failed = queue.claim().unwrap();
        assert_eq!(failed.kind, Kind::Failed);
        assert!(!failed.ready);
        assert_eq!(
            failed.error.as_deref(),
            Some("root 한글 한 é 😀 is unavailable")
        );
    }
    #[test]
    fn stopping_does_not_publish_or_release_a_pending_generation() {
        let epoch = Uuid::from_u128(1);
        let mut queue = Coalescer::new(epoch);
        queue.mark(Kind::Changed, None);
        let first = queue.claim().unwrap();
        queue.mark(Kind::Changed, None);
        queue.status.stopping = true;
        assert!(!queue.acknowledge(epoch, first.generation));
        assert!(queue.claim().is_none());
    }

    #[cfg(windows)]
    mod native_checks {
        use super::*;
        use std::{
            fs,
            path::PathBuf,
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };

        const WAIT: Duration = Duration::from_secs(5);
        struct Directory(PathBuf);
        impl Directory {
            fn new() -> Self {
                let root =
                    std::env::temp_dir().join(format!("flowmux-watch-{}-한글", Uuid::new_v4()));
                fs::create_dir(&root).unwrap();
                Self(root)
            }
        }
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        fn until(mut condition: impl FnMut() -> bool) {
            let deadline = Instant::now() + WAIT;
            while !condition() {
                assert!(
                    Instant::now() < deadline,
                    "owned watcher condition timed out"
                );
                thread::sleep(Duration::from_millis(5));
            }
        }
        fn watcher(root: PathBuf) -> (Watcher, mpsc::Receiver<Notice>) {
            let (sender, receiver) = mpsc::channel();
            let watcher = Watcher::start(root, move |notice| {
                let _ = sender.send(notice);
            })
            .unwrap();
            (watcher, receiver)
        }
        fn stopped(watcher: &Watcher) {
            watcher.stop();
            until(|| watcher.status().stopped);
            let status = watcher.status();
            assert!(status.stopping);
            assert!(!status.ready);
            assert!(!status.cancellation_over_budget);
        }

        #[test]
        fn recursive_unicode_changes_wait_for_exact_ack_and_stop_drains_owned_io() {
            let directory = Directory::new();
            let (watcher, notices) = watcher(directory.0.clone());
            let initial = notices.recv_timeout(WAIT).unwrap();
            assert!(initial.ready);
            assert_eq!(initial.kind, Kind::Ready);
            let nested = directory.0.join("nested 한 é 😀");
            fs::create_dir(&nested).unwrap();
            for index in 0..20 {
                fs::write(nested.join(format!("{index} 한글.txt")), "한글 한 é 😀").unwrap();
            }
            until(|| watcher.status().generation > initial.generation);
            let observed = watcher.status().generation;
            assert!(matches!(notices.try_recv(), Err(mpsc::TryRecvError::Empty)));
            assert!(!watcher.acknowledge(Uuid::new_v4(), initial.generation));
            assert!(watcher.acknowledge(initial.epoch, initial.generation));
            let changed = notices.recv_timeout(WAIT).unwrap();
            assert!(changed.generation >= observed);
            assert!(matches!(changed.kind, Kind::Changed | Kind::Rescan));
            assert!(!watcher.acknowledge(initial.epoch, initial.generation));
            stopped(&watcher);
        }

        #[test]
        fn unavailable_root_reports_failure_without_claiming_readiness() {
            let directory = Directory::new();
            let (watcher, notices) = watcher(directory.0.join("missing 한"));
            let failed = notices.recv_timeout(WAIT).unwrap();
            assert_eq!(failed.kind, Kind::Failed);
            assert!(!failed.ready);
            assert!(failed
                .error
                .as_deref()
                .is_some_and(|error| error.contains("cannot resolve editor root")));
            assert!(watcher.acknowledge(failed.epoch, failed.generation));
            stopped(&watcher);
        }

        #[test]
        fn extended_unicode_root_arms_and_observes_atomic_replacement() {
            use std::os::windows::ffi::OsStrExt;
            let directory = Directory::new();
            let mut root = directory.0.clone();
            while root.as_os_str().encode_wide().count() < 300 {
                root.push("nested-한글-한-😀-directory");
            }
            fs::create_dir_all(&root).unwrap();
            let target = root.join("target é.txt");
            fs::write(&target, "original").unwrap();
            let (watcher, notices) = watcher(root.clone());
            let initial = notices.recv_timeout(WAIT).unwrap();
            assert!(initial.ready);
            assert!(watcher.acknowledge(initial.epoch, initial.generation));
            let replacement = root.join("replacement é.txt");
            fs::write(&replacement, "new 한글 한 é 😀").unwrap();
            fs::rename(&replacement, &target).unwrap();
            let changed = notices.recv_timeout(WAIT).unwrap();
            assert!(changed.generation > initial.generation);
            assert!(matches!(changed.kind, Kind::Changed | Kind::Rescan));
            assert_eq!(fs::read_to_string(&target).unwrap(), "new 한글 한 é 😀");
            stopped(&watcher);
        }
    }
}
