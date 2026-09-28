// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded, read-only preparation for native editor Open.
//!
//! The UI retains its reply, pane/workspace identity and the returned Ticket.
//! On Prepared it must remove the matching ticket, reject stale/cancelled/expired
//! requests, and revalidate its destination before creating or activating a view.
//! Carry Ticket.submitted/deadline into the subsequent editor initialization;
//! preparation does not grant another twelve seconds to the next stage.
//!
//! No App, WebView, COM object or IPC reply enters this worker. Cancellation is
//! cooperative: an OS filesystem call already running is allowed to finish.

use crate::editor;
use anyhow::{ensure, Context};
use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc, Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub const MAX_PENDING: usize = 8;
pub const OPEN_BUDGET: Duration = Duration::from_secs(12);

#[derive(Clone, Debug)]
pub struct Ticket {
    pub id: u64,
    pub submitted: Instant,
    pub deadline: Instant,
    cancelled: Arc<AtomicBool>,
}
impl Ticket {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Also check this on the UI thread: cancellation can race the final emit.
    pub fn is_expired(&self, now: Instant) -> bool {
        now >= self.deadline
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct OpenPaths {
    pub root: PathBuf,
    pub path: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PrepareError {
    Cancelled,
    DeadlineExceeded,
    Stopped,
    Resolve(String),
}
impl fmt::Display for PrepareError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("editor open preparation was cancelled"),
            Self::DeadlineExceeded => formatter
                .write_str("editor open preparation exceeded its original twelve-second deadline"),
            Self::Stopped => {
                formatter.write_str("editor open preparer stopped; queued work was cancelled")
            }
            Self::Resolve(reason) => formatter.write_str(reason),
        }
    }
}
impl std::error::Error for PrepareError {}

#[derive(Debug)]
pub struct Prepared {
    pub id: u64,
    pub result: Result<OpenPaths, PrepareError>,
    // Count posted-but-unhandled results as admitted work. Moving Prepared into
    // an App event keeps its slot occupied until that event is handled/dropped.
    _permit: Permit,
}

#[derive(Debug, Default)]
struct Shared {
    stopped: AtomicBool,
    admitted: AtomicUsize,
}

#[derive(Debug)]
struct Permit(Arc<Shared>);
impl Permit {
    fn acquire(shared: &Arc<Shared>) -> anyhow::Result<Self> {
        shared
            .admitted
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_PENDING).then_some(count + 1)
            })
            .map_err(|_| anyhow::anyhow!("editor open preparation is busy (limit eight)"))?;
        Ok(Self(shared.clone()))
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.admitted.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Job {
    ticket: Ticket,
    root: PathBuf,
    path: PathBuf,
    permit: Permit,
}

// The production resolver is fixed. Only cfg(test) can supply channel-gated
// operations; no delay flag, callback injection or test hook reaches the binary.
#[cfg(test)]
type RootResolver = Box<dyn Fn(&Path) -> anyhow::Result<PathBuf> + Send>;
#[cfg(test)]
type PathResolver = Box<dyn Fn(&Path, &Path) -> anyhow::Result<PathBuf> + Send>;

enum Resolver {
    Native,
    #[cfg(test)]
    Test {
        root: RootResolver,
        path: PathResolver,
    },
}
impl Resolver {
    fn root(&self, root: &Path) -> anyhow::Result<PathBuf> {
        match self {
            Self::Native => editor::canonical_root(root),
            #[cfg(test)]
            Self::Test { root: resolve, .. } => resolve(root),
        }
    }

    fn path(&self, root: &Path, path: &Path) -> anyhow::Result<PathBuf> {
        match self {
            Self::Native => editor::resolve_existing(root, path),
            #[cfg(test)]
            Self::Test { path: resolve, .. } => resolve(root, path),
        }
    }
}

pub struct OpenPreparer {
    sender: Option<mpsc::SyncSender<Job>>,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}
impl OpenPreparer {
    pub fn start(emit: impl Fn(Prepared) + Send + 'static) -> anyhow::Result<Self> {
        Self::spawn(Resolver::Native, emit)
    }

    fn spawn(resolver: Resolver, emit: impl Fn(Prepared) + Send + 'static) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel::<Job>(MAX_PENDING);
        let shared = Arc::new(Shared::default());
        let thread_shared = shared.clone();
        let thread = thread::Builder::new()
            .name("flowmux-editor-open".into())
            .spawn(move || {
                for job in receiver {
                    let result = prepare(&job, &thread_shared, &resolver);
                    emit(Prepared {
                        id: job.ticket.id,
                        result,
                        _permit: job.permit,
                    });
                }
            })
            .context("cannot start editor open preparation worker")?;
        Ok(Self {
            sender: Some(sender),
            shared,
            thread: Some(thread),
        })
    }

    /// Nonblocking admission. submitted belongs to the original Open request,
    /// not the instant at which a later UI callback happens to enqueue work.
    pub fn submit(
        &self,
        id: u64,
        root: PathBuf,
        path: PathBuf,
        submitted: Instant,
    ) -> anyhow::Result<Ticket> {
        ensure!(
            !self.shared.stopped.load(Ordering::Acquire),
            "editor open preparer is stopped"
        );
        ensure!(
            submitted <= Instant::now(),
            "editor open submission time is in the future"
        );
        // Pure validation bounds admitted path memory and does no filesystem I/O.
        editor::validate_path(&root)?;
        editor::validate_path(&path)?;
        let deadline = submitted
            .checked_add(OPEN_BUDGET)
            .context("editor open deadline overflow")?;
        let ticket = Ticket {
            id,
            submitted,
            deadline,
            cancelled: Arc::new(AtomicBool::new(false)),
        };
        let job = Job {
            ticket: ticket.clone(),
            root,
            path,
            permit: Permit::acquire(&self.shared)?,
        };
        self.sender
            .as_ref()
            .context("editor open preparer is stopped")?
            .try_send(job)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    anyhow::anyhow!("editor open preparation queue is full")
                }
                mpsc::TrySendError::Disconnected(_) => {
                    anyhow::anyhow!("editor open preparation worker stopped")
                }
            })?;
        Ok(ticket)
    }

    /// Includes executing, queued, cancelled-but-not-drained and posted results.
    pub fn pending(&self) -> usize {
        self.shared.admitted.load(Ordering::Acquire)
    }

    /// Never joins filesystem work on the caller/UI thread. Queued jobs still
    /// produce Stopped results so their ownership/permits unwind normally.
    pub fn shutdown(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        self.sender.take();
        self.thread.take();
    }
}
impl Drop for OpenPreparer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn check_at(ticket: &Ticket, shared: &Shared, now: Instant) -> Result<(), PrepareError> {
    if shared.stopped.load(Ordering::Acquire) {
        Err(PrepareError::Stopped)
    } else if ticket.is_cancelled() {
        Err(PrepareError::Cancelled)
    } else if ticket.is_expired(now) {
        Err(PrepareError::DeadlineExceeded)
    } else {
        Ok(())
    }
}

fn prepare(job: &Job, shared: &Shared, resolver: &Resolver) -> Result<OpenPaths, PrepareError> {
    check_at(&job.ticket, shared, Instant::now())?;
    let root = resolver.root(&job.root);
    check_at(&job.ticket, shared, Instant::now())?;
    let root = root.map_err(|error| PrepareError::Resolve(format!("{error:#}")))?;

    let path = resolver.path(&root, &job.path);
    check_at(&job.ticket, shared, Instant::now())?;
    let path = path.map_err(|error| PrepareError::Resolve(format!("{error:#}")))?;
    if !root.is_absolute() || !path.is_absolute() || !path.starts_with(&root) {
        return Err(PrepareError::Resolve(
            "prepared editor file is outside its absolute workspace root".into(),
        ));
    }
    Ok(OpenPaths { root, path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const WAIT: Duration = Duration::from_secs(5);

    struct Directory(PathBuf);
    impl Directory {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("flowmux-open-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn test_root() -> PathBuf {
        std::env::temp_dir().join("flowmux-open-unit-root")
    }

    fn resolver(
        root: impl Fn(&Path) -> anyhow::Result<PathBuf> + Send + 'static,
        path: impl Fn(&Path, &Path) -> anyhow::Result<PathBuf> + Send + 'static,
    ) -> Resolver {
        Resolver::Test {
            root: Box::new(root),
            path: Box::new(path),
        }
    }

    fn start(resolver: Resolver) -> (OpenPreparer, mpsc::Receiver<Prepared>) {
        let (send, receive) = mpsc::channel();
        let worker = OpenPreparer::spawn(resolver, move |prepared| {
            let _ = send.send(prepared);
        })
        .unwrap();
        (worker, receive)
    }

    fn submit(worker: &OpenPreparer, id: u64) -> Ticket {
        worker
            .submit(id, test_root(), PathBuf::from("file.txt"), Instant::now())
            .unwrap()
    }

    fn receive(receiver: &mpsc::Receiver<Prepared>) -> Prepared {
        receiver.recv_timeout(WAIT).unwrap()
    }

    #[test]
    fn invalid_admission_does_not_consume_capacity_or_block_the_next_request() {
        let (worker, responses) = start(resolver(
            |root| Ok(root.to_path_buf()),
            |root, path| Ok(root.join(path)),
        ));
        for (root, path) in [
            (PathBuf::new(), PathBuf::from("file.txt")),
            (test_root(), PathBuf::new()),
            (test_root(), PathBuf::from("bad\0file.txt")),
        ] {
            assert!(worker.submit(1, root, path, Instant::now()).is_err());
            assert_eq!(worker.pending(), 0);
        }
        assert!(worker
            .submit(
                1,
                test_root(),
                "file.txt".into(),
                Instant::now() + Duration::from_secs(60),
            )
            .is_err());
        assert_eq!(worker.pending(), 0);
        let ticket = submit(&worker, 2);
        let prepared = receive(&responses);
        assert_eq!(prepared.id, ticket.id);
        assert!(prepared.result.is_ok());
        drop(prepared);
        assert_eq!(worker.pending(), 0);
    }

    #[test]
    fn native_resolution_preserves_unicode_and_rejects_missing_or_outside_paths() {
        let directory = Directory::new();
        let root = directory.0.join("root 한글 한 😀");
        fs::create_dir(&root).unwrap();
        let name = "document é 한글 😀.txt";
        let path = root.join(name);
        fs::write(&path, "content").unwrap();
        let outside = directory.0.join("outside.txt");
        fs::write(&outside, "unchanged").unwrap();
        let (send, receive) = mpsc::channel();
        let worker = OpenPreparer::start(move |prepared| {
            let _ = send.send(prepared);
        })
        .unwrap();
        worker
            .submit(1, root.clone(), PathBuf::from(name), Instant::now())
            .unwrap();
        let prepared = receive.recv_timeout(WAIT).unwrap();
        assert_eq!(prepared.id, 1);
        assert_eq!(
            prepared.result,
            Ok(OpenPaths {
                root: editor::canonical_root(&root).unwrap(),
                path: editor::display_path(fs::canonicalize(&path).unwrap()),
            })
        );
        drop(prepared);
        worker
            .submit(2, root.clone(), outside.clone(), Instant::now())
            .unwrap();
        let prepared = receive.recv_timeout(WAIT).unwrap();
        assert!(
            matches!(&prepared.result, Err(PrepareError::Resolve(reason)) if reason.contains("outside"))
        );
        drop(prepared);
        worker
            .submit(3, root.join("missing"), path, Instant::now())
            .unwrap();
        assert!(matches!(
            receive.recv_timeout(WAIT).unwrap().result,
            Err(PrepareError::Resolve(_))
        ));
        assert_eq!(fs::read_to_string(outside).unwrap(), "unchanged");
        assert_eq!(worker.pending(), 0);
    }

    #[test]
    fn cap_includes_active_queued_and_posted_results_until_their_permits_drop() {
        let (entered, observed) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let first = AtomicBool::new(true);
        let (worker, responses) = start(resolver(
            move |root| {
                if first.swap(false, Ordering::SeqCst) {
                    entered.send(()).unwrap();
                    gate.recv_timeout(WAIT).unwrap();
                }
                Ok(root.to_path_buf())
            },
            |root, path| Ok(root.join(path)),
        ));
        submit(&worker, 1);
        observed.recv_timeout(WAIT).unwrap();
        for id in 2..=MAX_PENDING as u64 {
            submit(&worker, id);
        }
        assert_eq!(worker.pending(), MAX_PENDING);
        assert!(worker
            .submit(99, test_root(), "extra.txt".into(), Instant::now())
            .is_err());
        release.send(()).unwrap();
        let mut posted: Vec<_> = (0..MAX_PENDING).map(|_| receive(&responses)).collect();
        assert!(posted.iter().all(|result| result.result.is_ok()));
        assert_eq!(
            posted.iter().map(|result| result.id).collect::<Vec<_>>(),
            (1..=MAX_PENDING as u64).collect::<Vec<_>>()
        );
        assert_eq!(worker.pending(), MAX_PENDING);
        assert!(worker
            .submit(99, test_root(), "extra.txt".into(), Instant::now())
            .is_err());
        drop(posted.pop());
        assert_eq!(worker.pending(), MAX_PENDING - 1);
        submit(&worker, 99);
        let last = receive(&responses);
        assert_eq!(last.id, 99);
        drop(last);
        drop(posted);
        assert_eq!(worker.pending(), 0);
    }

    #[test]
    fn cancelling_queued_work_skips_its_resolver_without_refunding_it_early() {
        let (entered, observed) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let (worker, responses) = start(resolver(
            move |root| {
                count.fetch_add(1, Ordering::SeqCst);
                entered.send(()).unwrap();
                gate.recv_timeout(WAIT).unwrap();
                Ok(root.to_path_buf())
            },
            |root, path| Ok(root.join(path)),
        ));
        submit(&worker, 1);
        observed.recv_timeout(WAIT).unwrap();
        let queued = submit(&worker, 2);
        queued.cancel();
        assert_eq!(worker.pending(), 2);
        release.send(()).unwrap();
        assert!(receive(&responses).result.is_ok());
        let cancelled = receive(&responses);
        assert_eq!(cancelled.id, 2);
        assert_eq!(cancelled.result, Err(PrepareError::Cancelled));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        drop(cancelled);
        assert_eq!(worker.pending(), 0);
    }

    #[test]
    fn cancellation_after_root_resolution_prevents_file_resolution() {
        let (entered, observed) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let path_calls = Arc::new(AtomicUsize::new(0));
        let count = path_calls.clone();
        let (worker, responses) = start(resolver(
            move |root| {
                entered.send(()).unwrap();
                gate.recv_timeout(WAIT).unwrap();
                Ok(root.to_path_buf())
            },
            move |root, path| {
                count.fetch_add(1, Ordering::SeqCst);
                Ok(root.join(path))
            },
        ));
        let ticket = submit(&worker, 1);
        observed.recv_timeout(WAIT).unwrap();
        ticket.cancel();
        release.send(()).unwrap();
        assert_eq!(receive(&responses).result, Err(PrepareError::Cancelled));
        assert_eq!(path_calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancellation_after_file_resolution_never_publishes_success() {
        let (entered, observed) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let (worker, responses) = start(resolver(
            |root| Ok(root.to_path_buf()),
            move |root, path| {
                entered.send(()).unwrap();
                gate.recv_timeout(WAIT).unwrap();
                Ok(root.join(path))
            },
        ));
        let ticket = submit(&worker, 1);
        observed.recv_timeout(WAIT).unwrap();
        ticket.cancel();
        release.send(()).unwrap();
        assert_eq!(receive(&responses).result, Err(PrepareError::Cancelled));
    }

    #[test]
    fn expired_original_deadline_skips_io_and_has_an_exact_boundary() {
        let (worker, responses) = start(resolver(
            |_| panic!("expired request performed root I/O"),
            |_, _| panic!("expired request performed file I/O"),
        ));
        let submitted = Instant::now() - OPEN_BUDGET - Duration::from_secs(1);
        let ticket = worker
            .submit(1, test_root(), "file.txt".into(), submitted)
            .unwrap();
        assert_eq!(ticket.submitted, submitted);
        assert_eq!(ticket.deadline, submitted + OPEN_BUDGET);
        assert_eq!(
            receive(&responses).result,
            Err(PrepareError::DeadlineExceeded)
        );
        let shared = Shared::default();
        assert_eq!(
            check_at(&ticket, &shared, ticket.deadline - Duration::from_nanos(1)),
            Ok(())
        );
        assert_eq!(
            check_at(&ticket, &shared, ticket.deadline),
            Err(PrepareError::DeadlineExceeded)
        );
    }

    #[test]
    fn drop_returns_before_inflight_io_and_skips_every_queued_resolver() {
        let (entered, observed) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let (worker, responses) = start(resolver(
            move |root| {
                count.fetch_add(1, Ordering::SeqCst);
                entered.send(()).unwrap();
                gate.recv_timeout(WAIT).unwrap();
                Ok(root.to_path_buf())
            },
            |_, _| panic!("shutdown must stop before the next filesystem step"),
        ));
        submit(&worker, 1);
        observed.recv_timeout(WAIT).unwrap();
        for id in 2..=MAX_PENDING as u64 {
            submit(&worker, id);
        }
        let shared = worker.shared.clone();
        let (dropped, drop_seen) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(worker);
            dropped.send(()).unwrap();
        });
        // Release only after Drop has returned; a joining implementation fails
        // this handshake instead of passing a fragile elapsed-time assertion.
        drop_seen.recv_timeout(WAIT).unwrap();
        assert_eq!(shared.admitted.load(Ordering::Acquire), MAX_PENDING);
        release.send(()).unwrap();
        for id in 1..=MAX_PENDING as u64 {
            let prepared = receive(&responses);
            assert_eq!(prepared.id, id);
            assert_eq!(prepared.result, Err(PrepareError::Stopped));
        }
        dropper.join().unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    }

    #[test]
    fn dropped_posted_results_release_admission_and_shutdown_rejects_more_work() {
        let (released, observed) = mpsc::channel();
        let mut worker = OpenPreparer::spawn(
            resolver(
                |root| Ok(root.to_path_buf()),
                |root, path| Ok(root.join(path)),
            ),
            move |prepared| {
                drop(prepared);
                released.send(()).unwrap();
            },
        )
        .unwrap();
        submit(&worker, 1);
        observed.recv_timeout(WAIT).unwrap();
        assert_eq!(worker.pending(), 0);
        worker.shutdown();
        assert!(worker
            .submit(2, test_root(), "file.txt".into(), Instant::now())
            .is_err());
        assert_eq!(worker.pending(), 0);
    }
}
