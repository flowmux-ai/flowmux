// SPDX-License-Identifier: GPL-3.0-or-later
//! One host-wide file mutation, prepared without writes before explicit acceptance.
//! The admission follows the prepared handles and posted outcome until consumed.
//! Cancellation is cooperative; dropping this service never joins blocked OS I/O.

use crate::{files_model::short_error, files_operation_fs as disk};
use anyhow::{ensure, Context};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex, Weak,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use uuid::Uuid;

pub const PREPARE_BUDGET: Duration = Duration::from_secs(4);
pub const COMMIT_BUDGET: Duration = Duration::from_secs(30);
pub const MAX_ADMITTED: usize = 1;
// ponytail: one operation per host; add per-root admission only if measured
// throughput requires it. The editor reservation shares this same ceiling.

#[derive(Clone)]
pub struct Ticket {
    pub id: Uuid,
    cancelled: Arc<AtomicBool>,
    deadline: Instant,
}
impl Ticket {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    /// Preparation expiry only. An accepted commit has its own bounded budget.
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.deadline
    }
    fn check(&self) -> anyhow::Result<()> {
        ensure!(
            !self.is_cancelled(),
            "Files operation was cancelled before acceptance"
        );
        ensure!(
            !self.is_expired(),
            "Files operation preparation exceeded its original four-second deadline"
        );
        Ok(())
    }
    fn control(&self, deadline: Instant) -> disk::Control {
        disk::Control {
            cancelled: self.cancelled.clone(),
            deadline,
        }
    }
}
struct Shared {
    admitted: AtomicUsize,
    stopped: AtomicBool,
    current: Mutex<Weak<AtomicBool>>,
}
struct Permit(Arc<Shared>);
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.admitted.fetch_sub(1, Ordering::AcqRel);
    }
}
pub struct Ready {
    pub id: Uuid,
    pub error: Option<String>,
    prepared: Option<Box<disk::Prepared>>,
    ticket: Ticket,
    permit: Permit,
}
pub struct Finished {
    pub id: Uuid,
    pub result: Result<disk::Outcome, String>,
    _permit: Permit,
}
pub enum Event {
    Prepared(Ready),
    Finished(Finished),
}
enum Work {
    Prepare(disk::Request, Ticket, Permit),
    Commit(Ready, Instant),
}
pub struct Service {
    sender: SyncSender<Work>,
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
type Prepare = dyn Fn(disk::Request, &disk::Control) -> anyhow::Result<disk::Prepared> + Send;
impl Service {
    pub fn start(emit: impl Fn(Event) + Send + 'static) -> anyhow::Result<Self> {
        Self::start_with(emit, disk::Prepared::prepare)
    }
    fn start_with(
        emit: impl Fn(Event) + Send + 'static,
        prepare: impl Fn(disk::Request, &disk::Control) -> anyhow::Result<disk::Prepared>
            + Send
            + 'static,
    ) -> anyhow::Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let shared = Arc::new(Shared {
            admitted: AtomicUsize::new(0),
            stopped: AtomicBool::new(false),
            current: Mutex::new(Weak::new()),
        });
        let worker = thread::Builder::new()
            .name("flowmux-files-actions".into())
            .spawn(move || worker(receiver, Box::new(prepare), emit))
            .context("cannot start Files operation worker")?;
        Ok(Self {
            sender,
            shared,
            worker: Some(worker),
        })
    }
    pub fn is_active(&self) -> bool {
        self.shared.admitted.load(Ordering::Acquire) != 0
    }
    pub fn prepare(&self, request: disk::Request, submitted: Instant) -> anyhow::Result<Ticket> {
        ensure!(
            !self.shared.stopped.load(Ordering::Acquire),
            "Files operations are stopped"
        );
        let ticket = Ticket {
            id: Uuid::new_v4(),
            cancelled: Arc::new(AtomicBool::new(false)),
            deadline: submitted + PREPARE_BUDGET,
        };
        ticket.check()?;
        // Bound strings on the caller before transferring ownership to the worker.
        ensure!(
            request.source.len() <= crate::files_model::MAX_PATH_BYTES
                && request.destination.len() <= crate::files_model::MAX_PATH_BYTES
                && request.root.as_os_str().len() <= crate::files_model::MAX_PATH_BYTES,
            "Files operation paths exceed the input limit"
        );
        self.shared
            .admitted
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                anyhow::anyhow!("another Files operation is active; inspect its status first")
            })?;
        let permit = Permit(self.shared.clone());
        *self
            .shared
            .current
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Arc::downgrade(&ticket.cancelled);
        self.sender
            .try_send(Work::Prepare(request, ticket.clone(), permit))
            .map_err(|_| anyhow::anyhow!("Files operation worker is unavailable"))?;
        Ok(ticket)
    }
    /// Call only after the host has recorded and exposed its acceptance receipt.
    /// Failure here is a real failed receipt, never permission to replay silently.
    pub fn commit(&self, ready: Ready) -> anyhow::Result<()> {
        ensure!(
            Arc::ptr_eq(&self.shared, &ready.permit.0),
            "Files preparation belongs to another service"
        );
        ensure!(
            !self.shared.stopped.load(Ordering::Acquire),
            "Files operations are stopped"
        );
        ready.ticket.check()?;
        ensure!(
            ready.prepared.is_some() && ready.error.is_none(),
            "Files preparation did not succeed"
        );
        self.sender
            .try_send(Work::Commit(ready, Instant::now() + COMMIT_BUDGET))
            .map_err(|_| anyhow::anyhow!("Files operation worker stopped before commit"))?;
        Ok(())
    }
}
impl Drop for Service {
    fn drop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        if let Some(cancelled) = self
            .shared
            .current
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .upgrade()
        {
            cancelled.store(true, Ordering::Release);
        }
        if let Some(worker) = self.worker.take() {
            if worker.is_finished() {
                let _ = worker.join();
            }
            // Dropping the sender wakes an idle worker; a running OS call retains
            // its handles/permit and cannot be forcefully detached from ownership.
        }
    }
}
fn worker(receiver: Receiver<Work>, prepare: Box<Prepare>, emit: impl Fn(Event)) {
    while let Ok(work) = receiver.recv() {
        let event = match work {
            Work::Prepare(request, ticket, permit) => {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ticket.check()?;
                    let prepared = prepare(request, &ticket.control(ticket.deadline))?;
                    ticket.check()?;
                    Ok::<_, anyhow::Error>(prepared)
                }))
                .unwrap_or_else(|_| Err(anyhow::anyhow!("Files preparation panicked")));
                let (prepared, error) = match result {
                    Ok(prepared) => (Some(Box::new(prepared)), None),
                    Err(error) => (None, Some(short_error(error))),
                };
                Event::Prepared(Ready {
                    id: ticket.id,
                    error,
                    prepared,
                    ticket,
                    permit,
                })
            }
            Work::Commit(mut ready, deadline) => {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    ready
                        .prepared
                        .take()
                        .context("Files preparation missing")?
                        .commit(&ready.ticket.control(deadline))
                }))
                .unwrap_or_else(|_| {
                    Err(anyhow::anyhow!(
                        "Files operation panicked; inspect the destination before retrying"
                    ))
                })
                .map_err(short_error);
                Event::Finished(Finished {
                    id: ready.id,
                    result,
                    _permit: ready.permit,
                })
            }
        };
        // A lost consumer must release its owned handles/permit rather than kill
        // this worker and strand subsequent requests.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| emit(event)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> disk::Request {
        disk::Request {
            kind: disk::Kind::Copy,
            root: std::path::PathBuf::from("C:\\owned"),
            source: "한글.txt".into(),
            destination: "한.txt".into(),
        }
    }
    #[test]
    fn expired_admission_does_not_occupy_the_worker() {
        let service =
            Service::start_with(|_| {}, |_, _| anyhow::bail!("injected preparation refusal"))
                .unwrap();
        assert!(service
            .prepare(request(), Instant::now() - PREPARE_BUDGET)
            .is_err());
        assert!(!service.is_active());
    }
    #[test]
    fn delivered_preparation_holds_admission_until_consumed_even_when_failed() {
        let (tx, rx) = mpsc::channel();
        let service = Service::start_with(
            move |event| tx.send(event).unwrap(),
            |_, _| anyhow::bail!("injected preparation refusal"),
        )
        .unwrap();
        let ticket = service.prepare(request(), Instant::now()).unwrap();
        let event = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let Event::Prepared(ready) = event else {
            panic!("expected preparation");
        };
        assert_eq!(ready.id, ticket.id);
        assert_eq!(ready.error.as_deref(), Some("injected preparation refusal"));
        assert!(service.is_active());
        assert!(service.prepare(request(), Instant::now()).is_err());
        assert!(service.commit(ready).is_err());
        assert!(!service.is_active());
    }
    #[test]
    fn cancellation_and_service_drop_retain_running_admission() {
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let service = Service::start_with(
            move |event| done_tx.send(event).unwrap(),
            move |_, control| {
                started_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                ensure!(
                    control.cancelled.load(Ordering::Acquire),
                    "expected cancellation"
                );
                anyhow::bail!("cancelled during preparation")
            },
        )
        .unwrap();
        let ticket = service.prepare(request(), Instant::now()).unwrap();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        ticket.cancel();
        assert!(service.is_active());
        drop(service); // Must not join the worker that waits for this test thread.
        release_tx.send(()).unwrap();
        let Event::Prepared(ready) = done_rx.recv_timeout(Duration::from_secs(2)).unwrap() else {
            panic!("expected preparation");
        };
        assert_eq!(ready.error.as_deref(), Some("cancelled during preparation"));
        assert_eq!(ready.permit.0.admitted.load(Ordering::Acquire), 1);
        let shared = ready.permit.0.clone();
        drop(ready);
        assert_eq!(shared.admitted.load(Ordering::Acquire), 0);
    }
}
