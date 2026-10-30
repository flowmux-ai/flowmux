// SPDX-License-Identifier: GPL-3.0-or-later
//! Coalesced terminal state delivery, independent of best-effort notifications.

use flowmux_ipc::protocol::TerminalProgramSnapshot;
use flowmux_notify::program_status::{parse_program_status, ProgramRecords, ProgramStatus};
use flowmux_notify::OscExtractor;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

#[derive(Default)]
struct State {
    records: ProgramRecords,
    revision: u64,
    delivered: u64,
}

pub(super) struct ProgramStatusShared {
    generation: Option<uuid::Uuid>,
    enabled: AtomicBool,
    state: Mutex<State>,
}

impl ProgramStatusShared {
    pub fn new(generation: Option<uuid::Uuid>) -> Self {
        Self {
            generation,
            enabled: AtomicBool::new(generation.is_some()),
            state: Mutex::new(State::default()),
        }
    }

    fn report(&self, payload: &str) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        if payload == "133;A" || payload.starts_with("133;A;") {
            self.process_boundary();
        } else if let Some(ProgramStatus::Report(report)) = parse_program_status(payload) {
            let mut state = self.state.lock().unwrap();
            state.records.apply(report);
            state.revision += 1;
        }
    }

    pub fn process_boundary(&self) {
        let mut state = self.state.lock().unwrap();
        let count = state.records.records().count();
        state.records.process_boundary();
        if count != state.records.records().count() {
            state.revision += 1;
        }
    }

    fn reset(&self) {
        let mut state = self.state.lock().unwrap();
        if state.records.records().next().is_some() {
            state.records.reset();
            state.revision += 1;
        }
    }

    pub fn pending(&self) -> Option<TerminalProgramSnapshot> {
        if !self.enabled.load(Ordering::Acquire) {
            return None;
        }
        let state = self.state.lock().unwrap();
        (state.revision > state.delivered).then(|| TerminalProgramSnapshot {
            generation: self.generation.unwrap(),
            owner_pid: std::process::id(),
            revision: state.revision,
            records: state
                .records
                .records()
                .map(|record| record.to_payload())
                .collect(),
        })
    }

    pub fn delivered(&self, revision: u64) {
        let mut state = self.state.lock().unwrap();
        state.delivered = state.delivered.max(revision);
    }

    pub fn disable(&self) {
        self.enabled.store(false, Ordering::Release);
    }
}

pub(super) type OscCallback = Box<dyn FnMut(&str)>;

pub(super) struct ProgramStatusStream {
    parser: OscExtractor<OscCallback>,
}

impl ProgramStatusStream {
    pub fn new(shared: Arc<ProgramStatusShared>) -> Self {
        let reports = shared.clone();
        let callback: OscCallback = Box::new(move |payload| reports.report(payload));
        let parser = OscExtractor::new(callback).with_full_reset_callback(move || shared.reset());
        Self { parser }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.feed(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observer() -> (Arc<ProgramStatusShared>, ProgramStatusStream) {
        let shared = Arc::new(ProgramStatusShared::new(Some(uuid::Uuid::new_v4())));
        let stream = ProgramStatusStream::new(shared.clone());
        (shared, stream)
    }

    #[test]
    fn coalesces_all_records_without_losing_later_updates_during_delivery() {
        let (shared, mut stream) = observer();
        stream.feed(b"\x1b]7501;state=working\x07\x1b]7501;state=blocked:id=child:kind=auth\x07");
        let in_flight = shared.pending().unwrap();
        stream.feed(b"\x1b]7501;state=done\x07\x1b]7501;state=clear:id=child\x07");
        shared.delivered(in_flight.revision);
        let newest = shared.pending().unwrap();
        assert!(newest.revision > in_flight.revision);
        assert_eq!(newest.records, ["7501;state=done"]);
        shared.delivered(newest.revision);
        assert!(shared.pending().is_none());
        // An older acknowledgement cannot roll back the delivered watermark.
        shared.delivered(in_flight.revision);
        assert!(shared.pending().is_none());
    }

    #[test]
    fn lifetime_events_and_screen_changes_preserve_order_for_every_split() {
        let bytes = b"\x1b]7501;state=done:id=result\x07\x1b]7501;state=blocked:id=wait\x07\x1b]133;A;cl=line\x07\x1b[?1049h\x1b[!p\x1b[?1049l";
        for split in 0..=bytes.len() {
            let (shared, mut stream) = observer();
            stream.feed(&bytes[..split]);
            stream.feed(&bytes[split..]);
            assert_eq!(
                shared.pending().unwrap().records,
                ["7501;state=done:id=result"]
            );
            stream.feed(b"\x1bc\x1b]7501;state=error:id=new\x07");
            assert_eq!(
                shared.pending().unwrap().records,
                ["7501;state=error:id=new"]
            );
        }
    }

    #[test]
    fn exit_and_full_reset_deliver_removal_even_without_more_output() {
        let (shared, mut stream) = observer();
        stream.feed(b"\x1b]7501;state=working\x07");
        shared.delivered(shared.pending().unwrap().revision);
        shared.process_boundary();
        let exit = shared.pending().unwrap();
        assert!(exit.records.is_empty());
        shared.delivered(exit.revision);
        shared.process_boundary();
        assert!(shared.pending().is_none());
        stream.feed(b"\x1b]7501;state=done\x07");
        shared.delivered(shared.pending().unwrap().revision);
        shared.process_boundary();
        assert!(shared.pending().is_none(), "done survives process exit");
        stream.feed(b"\x1bc");
        assert!(shared.pending().unwrap().records.is_empty());
    }

    #[test]
    fn unsupported_context_and_probes_never_create_a_snapshot() {
        let shared = Arc::new(ProgramStatusShared::new(None));
        let mut stream = ProgramStatusStream::new(shared.clone());
        stream.feed(b"\x1b]7501;?\x07\x1b]7501;state=working\x07");
        assert!(shared.pending().is_none());
        let (shared, mut stream) = observer();
        stream.feed(b"\x1b]7501;?\x07\x1b]7501;state=done:msg=AA==\x07");
        assert!(shared.pending().is_none());
        shared.disable();
        stream.feed(b"\x1b]7501;state=working\x07");
        assert!(shared.pending().is_none());
    }

    #[test]
    fn stalled_delivery_keeps_a_bounded_complete_snapshot() {
        let (shared, mut stream) = observer();
        for id in 0..1000 {
            stream.feed(format!("\x1b]7501;state=blocked:id=job{id}:kind=question\x07").as_bytes());
        }
        let snapshot = shared.pending().unwrap();
        assert_eq!(snapshot.records.len(), 256);
        assert!(snapshot.records[0].contains("id=job744:"));
        stream.feed(b"\x1b]7501;state=clear\x07\x1b]7501;state=error\x07");
        assert_eq!(shared.pending().unwrap().records, ["7501;state=error"]);
    }
}
