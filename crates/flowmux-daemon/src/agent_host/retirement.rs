// SPDX-License-Identifier: GPL-3.0-or-later
//! Retirement eligibility, never process termination. Provider adapters must
//! verify whole-work completion and atomically accept idle-only shutdown.

use flowmux_core::AgentInstanceId;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkDisposition {
    /// Includes permission, input and retry waits and outstanding child work.
    Working,
    VerifiedComplete,
    Unknown,
}

#[derive(Debug)]
struct Work {
    generation: u64,
    disposition: WorkDisposition,
    completed_at: Option<Instant>,
    revision: u64,
}

/// A local recheck token. It is not permission to signal or kill a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetirementCandidate {
    pub instance: AgentInstanceId,
    pub generation: u64,
    work_revision: u64,
    connection_revision: u64,
    deadline: Instant,
}

/// Host-local monotonic state. No UI status, seen flag, hook timestamp or
/// persisted deadline is accepted as completion evidence.
pub struct RetirementPolicy {
    work: HashMap<AgentInstanceId, Work>,
    gui_connections: HashSet<u64>,
    disconnected_at: Instant,
    connection_revision: u64,
    timeout: Duration,
}

impl RetirementPolicy {
    pub fn new(timeout: Duration, now: Instant) -> Self {
        Self {
            work: HashMap::new(),
            gui_connections: HashSet::new(),
            disconnected_at: now,
            connection_revision: 0,
            timeout,
        }
    }

    /// Call before releasing the PTY spawn gate. Duplicate launch requests do
    /// not reset an existing instance's generation or completion record.
    pub fn register(&mut self, instance: AgentInstanceId) {
        self.work.entry(instance).or_insert(Work {
            generation: 0,
            disposition: WorkDisposition::Unknown,
            completed_at: None,
            revision: 0,
        });
    }

    /// Remove only after exit and owned descendant cleanup have completed.
    pub fn remove_reaped(&mut self, instance: AgentInstanceId) {
        self.work.remove(&instance);
    }

    /// Connection IDs are assigned by the Host, not supplied by clients.
    /// A CLI, hook or terminal relay connection must not call this method.
    pub fn gui_connected(&mut self, connection: u64) {
        if self.gui_connections.insert(connection) {
            self.connection_revision += 1;
        }
    }

    pub fn gui_disconnected(&mut self, connection: u64, now: Instant) {
        if self.gui_connections.remove(&connection) {
            self.connection_revision += 1;
            if self.gui_connections.is_empty() {
                self.disconnected_at = now;
            }
        }
    }

    /// Recompute from the original completion/disconnect times. A settings
    /// change is not a fresh completion and does not grant another full period.
    pub fn set_timeout(&mut self, timeout: Duration) {
        if self.timeout != timeout {
            self.timeout = timeout;
            self.connection_revision += 1;
        }
    }

    /// The normalizer assigns a strictly newer generation only for verified
    /// new work or explicit continuation, never for a larger hook sequence.
    pub fn work_started(&mut self, instance: AgentInstanceId, generation: u64) -> bool {
        let Some(work) = self.work.get_mut(&instance) else {
            return false;
        };
        if generation <= work.generation {
            return false;
        }
        work.generation = generation;
        work.disposition = WorkDisposition::Working;
        work.completed_at = None;
        work.revision += 1;
        true
    }

    /// Require complete provider coverage, no outstanding work/waits and no
    /// observation gaps. A root Stop or idle screen alone cannot call this.
    /// Generation zero permits a verified ready agent that has done no work.
    pub fn completion_verified(
        &mut self,
        instance: AgentInstanceId,
        generation: u64,
        now: Instant,
    ) -> bool {
        let Some(work) = self.work.get_mut(&instance) else {
            return false;
        };
        if work.generation != generation {
            return false;
        }
        if work.disposition != WorkDisposition::VerifiedComplete {
            work.disposition = WorkDisposition::VerifiedComplete;
            // Revalidation of noise restores the original deadline.
            work.completed_at.get_or_insert(now);
            work.revision += 1;
        }
        true
    }

    /// Ambiguous activity, observation loss or conflicting evidence blocks
    /// retirement without destroying the previous verified completion time.
    pub fn observation_uncertain(&mut self, instance: AgentInstanceId) {
        if let Some(work) = self.work.get_mut(&instance) {
            if work.disposition != WorkDisposition::Unknown {
                work.disposition = WorkDisposition::Unknown;
                work.revision += 1;
            }
        }
    }

    pub fn disposition(&self, instance: AgentInstanceId) -> Option<WorkDisposition> {
        self.work.get(&instance).map(|work| work.disposition)
    }

    pub fn deadline(&self, instance: AgentInstanceId) -> Option<Instant> {
        if !self.gui_connections.is_empty() {
            return None;
        }
        let work = self.work.get(&instance)?;
        if work.disposition != WorkDisposition::VerifiedComplete {
            return None;
        }
        work.completed_at?
            .max(self.disconnected_at)
            .checked_add(self.timeout)
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.work.keys().filter_map(|id| self.deadline(*id)).min()
    }

    pub fn due(&self, now: Instant) -> Vec<RetirementCandidate> {
        self.work
            .iter()
            .filter_map(|(&instance, work)| {
                let deadline = self.deadline(instance)?;
                (deadline <= now).then_some(RetirementCandidate {
                    instance,
                    generation: work.generation,
                    work_revision: work.revision,
                    connection_revision: self.connection_revision,
                    deadline,
                })
            })
            .collect()
    }

    /// Recheck on the Host's serialized state path immediately before asking
    /// the provider to atomically reject shutdown if it has started any work.
    pub fn still_eligible(&self, candidate: RetirementCandidate, now: Instant) -> bool {
        self.connection_revision == candidate.connection_revision
            && self.work.get(&candidate.instance).is_some_and(|work| {
                work.generation == candidate.generation && work.revision == candidate.work_revision
            })
            && self.deadline(candidate.instance) == Some(candidate.deadline)
            && now >= candidate.deadline
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINUTE: Duration = Duration::from_secs(60);

    fn fixture() -> (RetirementPolicy, AgentInstanceId, Instant) {
        let now = Instant::now();
        let mut policy = RetirementPolicy::new(30 * MINUTE, now);
        let id = AgentInstanceId::new();
        policy.register(id);
        (policy, id, now)
    }

    #[test]
    fn working_waiting_and_unknown_never_expire() {
        let (mut policy, id, now) = fixture();
        assert!(policy.due(now + 365 * 24 * 60 * MINUTE).is_empty());
        assert!(policy.work_started(id, 1));
        assert!(policy.due(now + 365 * 24 * 60 * MINUTE).is_empty());
        policy.observation_uncertain(id);
        assert_eq!(policy.disposition(id), Some(WorkDisposition::Unknown));
        assert_eq!(policy.next_deadline(), None);
    }

    #[test]
    fn idle_before_disconnect_gets_full_period_from_last_gui() {
        let (mut policy, id, now) = fixture();
        policy.gui_connected(1);
        policy.gui_connected(2);
        policy.completion_verified(id, 0, now);
        policy.gui_disconnected(1, now + 40 * MINUTE);
        assert!(policy.due(now + 100 * MINUTE).is_empty());
        policy.gui_disconnected(2, now + 50 * MINUTE);
        // Duplicate disconnect must not extend the period.
        policy.gui_disconnected(2, now + 60 * MINUTE);
        assert!(policy
            .due(now + 80 * MINUTE - Duration::from_nanos(1))
            .is_empty());
        let candidate = policy.due(now + 80 * MINUTE)[0];
        assert!(policy.still_eligible(candidate, now + 80 * MINUTE));
    }

    #[test]
    fn completion_after_disconnect_and_duplicates_keep_original_deadline() {
        let (mut policy, id, now) = fixture();
        policy.gui_connected(1);
        policy.work_started(id, 1);
        policy.gui_disconnected(1, now + MINUTE);
        policy.completion_verified(id, 1, now + 10 * MINUTE);
        policy.completion_verified(id, 1, now + 20 * MINUTE);
        policy.register(id);
        assert_eq!(policy.deadline(id), Some(now + 40 * MINUTE));
        assert!(!policy.work_started(id, 1));
        assert!(!policy.completion_verified(id, 0, now + 25 * MINUTE));
        assert_eq!(policy.deadline(id), Some(now + 40 * MINUTE));
    }

    #[test]
    fn uncertainty_blocks_expiry_then_noise_restores_original_deadline() {
        let (mut policy, id, now) = fixture();
        policy.completion_verified(id, 0, now);
        let stale = policy.due(now + 30 * MINUTE)[0];
        policy.observation_uncertain(id);
        assert!(!policy.still_eligible(stale, now + 40 * MINUTE));
        assert!(policy.due(now + 40 * MINUTE).is_empty());
        policy.completion_verified(id, 0, now + 40 * MINUTE);
        assert_eq!(policy.deadline(id), Some(now + 30 * MINUTE));
        assert!(!policy.still_eligible(stale, now + 40 * MINUTE));
        assert_eq!(policy.due(now + 40 * MINUTE).len(), 1);
    }

    #[test]
    fn real_continuation_invalidates_completion_and_pending_expiry() {
        let (mut policy, id, now) = fixture();
        policy.work_started(id, 1);
        policy.completion_verified(id, 1, now);
        let stale = policy.due(now + 30 * MINUTE)[0];
        policy.work_started(id, 2);
        assert!(!policy.still_eligible(stale, now + 30 * MINUTE));
        assert!(!policy.completion_verified(id, 1, now + 31 * MINUTE));
        policy.completion_verified(id, 2, now + 35 * MINUTE);
        assert_eq!(policy.deadline(id), Some(now + 65 * MINUTE));
    }

    #[test]
    fn reconnect_and_config_changes_invalidate_callbacks() {
        let (mut policy, id, now) = fixture();
        policy.completion_verified(id, 0, now);
        let stale = policy.due(now + 30 * MINUTE)[0];
        policy.gui_connected(1);
        assert!(!policy.still_eligible(stale, now + 30 * MINUTE));
        policy.gui_disconnected(1, now + 31 * MINUTE);
        assert_eq!(policy.deadline(id), Some(now + 61 * MINUTE));
        policy.set_timeout(5 * MINUTE);
        assert_eq!(policy.deadline(id), Some(now + 36 * MINUTE));
        let candidate = policy.due(now + 36 * MINUTE)[0];
        policy.set_timeout(60 * MINUTE);
        assert!(!policy.still_eligible(candidate, now + 36 * MINUTE));
        assert_eq!(policy.deadline(id), Some(now + 91 * MINUTE));
    }

    #[test]
    fn reaped_instance_and_old_generation_cannot_be_resurrected() {
        let (mut policy, id, now) = fixture();
        policy.completion_verified(id, 0, now);
        let candidate = policy.due(now + 30 * MINUTE)[0];
        policy.remove_reaped(id);
        assert!(!policy.still_eligible(candidate, now + 30 * MINUTE));
        assert!(!policy.work_started(id, 2));
        assert!(!policy.completion_verified(id, 0, now + 30 * MINUTE));
        assert_eq!(policy.next_deadline(), None);
    }
}
