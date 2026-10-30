// SPDX-License-Identifier: GPL-3.0-or-later
//! Bounded OSC 7501 revision 0.4 parsing and per-terminal records.
//!
//! This module does not advertise support, answer probes, infer agent identity,
//! or resolve native-hook requests. Callers must supply terminal lifecycle events.

use std::collections::VecDeque;

use base64::{engine::general_purpose, Engine as _};

/// Includes the OSC number and separator, excludes framing. Reserve four bytes
/// for ESC ] and ESC \\ even when the sender uses the shorter BEL terminator.
pub const MAX_PROGRAM_STATUS_PAYLOAD: usize = 4092;
pub const MAX_PROGRAM_STATUS_RECORDS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    Clear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockedKind {
    Permission,
    Question,
    Auth,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramReport {
    pub state: ProgramState,
    /// None addresses the root; an empty or malformed id never does.
    pub id: Option<String>,
    pub kind: Option<BlockedKind>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgramStatus {
    Probe,
    Report(ProgramReport),
}

/// Parse the payload supplied by [`crate::OscExtractor`]. Malformed pairs are
/// skipped; limits and invalid encoded text reject the entire report, including
/// invalid values that a later duplicate would otherwise replace.
pub fn parse_program_status(payload: &str) -> Option<ProgramStatus> {
    if payload.len() > MAX_PROGRAM_STATUS_PAYLOAD {
        return None;
    }
    let body = payload.strip_prefix("7501;")?;
    if body == "?" {
        return Some(ProgramStatus::Probe);
    }
    let mut state = None;
    let mut id = None;
    let mut kind = None;
    let mut progress = None;
    let mut app = None;
    let mut title = None;
    let mut message = None;
    for pair in body.split(':') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim_ascii(), value.trim_ascii());
        if key.len() > 16 {
            return None;
        }
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_lowercase()) {
            continue;
        }
        // Check limits before syntax: an oversized field must not become a
        // harmless malformed pair and partially apply the rest of the report.
        match key {
            "msg" if value.len() > 2732 => return None,
            "title" if value.len() > 256 => return None,
            "app" if value.len() > 32 => return None,
            "id" if value.len() > 128
                || value.split('/').count() > 8
                || value.split('/').any(|part| part.len() > 32) =>
            {
                return None;
            }
            _ => {}
        }
        // An explicitly supplied invalid id must never fall back to the root.
        if key == "id" {
            id = Some(value);
        }
        if !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b))
        {
            continue;
        }
        match key {
            "state" => state = Some(value),
            "id" => id = Some(value),
            "kind" => kind = Some(value),
            "progress" => progress = Some(value),
            "app" => app = Some(value),
            "title" => title = Some(decode_text(value, 192)?),
            "msg" => message = Some(decode_text(value, 2048)?),
            _ => {}
        }
    }
    let state = match state? {
        "idle" => ProgramState::Idle,
        "working" => ProgramState::Working,
        "done" => ProgramState::Done,
        "blocked" => ProgramState::Blocked,
        "error" => ProgramState::Error,
        "clear" => ProgramState::Clear,
        _ => return None,
    };
    if id.is_some_and(|id| !id.split('/').all(valid_segment)) {
        return None;
    }
    let kind = if state == ProgramState::Blocked {
        match kind {
            Some("permission") => Some(BlockedKind::Permission),
            Some("question") => Some(BlockedKind::Question),
            Some("auth") => Some(BlockedKind::Auth),
            _ => None,
        }
    } else {
        None
    };
    let progress = if matches!(state, ProgramState::Working | ProgramState::Blocked) {
        progress
            .filter(|value| !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|value| value.parse::<u8>().ok())
            .filter(|value| *value <= 100)
    } else {
        None
    };
    Some(ProgramStatus::Report(ProgramReport {
        state,
        id: id.map(str::to_owned),
        kind,
        progress,
        app: app.filter(|value| valid_segment(value)).map(str::to_owned),
        title,
        message,
    }))
}

fn valid_segment(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

fn decode_text(value: &str, max_decoded: usize) -> Option<String> {
    // Standard alphabet only, accepting either fully padded or unpadded input.
    let bytes = general_purpose::STANDARD
        .decode(value)
        .or_else(|_| general_purpose::STANDARD_NO_PAD.decode(value))
        .ok()?;
    if bytes.len() > max_decoded {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    (!text.chars().any(char::is_control)).then_some(text)
}

/// Least-recently-updated eviction, independently owned by one terminal. There
/// is intentionally no clock or heartbeat expiry. Screen switches do nothing.
#[derive(Debug, Default)]
pub struct ProgramRecords {
    records: VecDeque<ProgramReport>,
}

impl ProgramRecords {
    pub fn records(&self) -> impl Iterator<Item = &ProgramReport> {
        self.records.iter()
    }

    /// Apply only a fully validated report from [`parse_program_status`].
    pub fn apply(&mut self, report: ProgramReport) {
        if report.state == ProgramState::Clear {
            match report.id.as_deref() {
                None => self.reset(),
                Some(id) => self.records.retain(|record| {
                    !record.id.as_deref().is_some_and(|candidate| {
                        candidate == id
                            || candidate
                                .strip_prefix(id)
                                .is_some_and(|suffix| suffix.starts_with('/'))
                    })
                }),
            }
            return;
        }
        self.records.retain(|record| record.id != report.id);
        if self.records.len() == MAX_PROGRAM_STATUS_RECORDS {
            self.records.pop_front();
        }
        self.records.push_back(report);
    }

    pub fn get(&self, id: Option<&str>) -> Option<&ProgramReport> {
        self.records
            .iter()
            .find(|record| record.id.as_deref() == id)
    }

    /// Resolve inheritance at read time; replacing an ancestor's app immediately
    /// affects descendants, including paths whose intermediate parents are absent.
    pub fn app(&self, mut id: Option<&str>) -> Option<&str> {
        loop {
            if let Some(app) = self.get(id).and_then(|record| record.app.as_deref()) {
                return Some(app);
            }
            id = id?.rsplit_once('/').map(|(parent, _)| parent);
        }
    }

    /// Attached-process exit or OSC 133 A. Idle records are also dropped so a
    /// previous program cannot identify the shell's next command.
    pub fn process_boundary(&mut self) {
        self.records
            .retain(|record| matches!(record.state, ProgramState::Done | ProgramState::Error));
    }

    /// Full terminal reset (RIS), never a soft reset or screen switch.
    pub fn reset(&mut self) {
        self.records.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse_osc, OscExtractor};

    fn report(body: &str) -> ProgramReport {
        match parse_program_status(&format!("7501;{body}")).expect("valid report") {
            ProgramStatus::Report(report) => report,
            ProgramStatus::Probe => panic!("unexpected probe"),
        }
    }

    fn rejected(body: &str) {
        assert!(
            parse_program_status(&format!("7501;{body}")).is_none(),
            "{body}"
        );
    }

    #[test]
    fn probe_is_separate_from_reports_and_notifications() {
        assert_eq!(parse_program_status("7501;?"), Some(ProgramStatus::Probe));
        for payload in ["7501;?:states=idle", "7501;", "7501; ?", "9;state=done"] {
            assert_eq!(parse_program_status(payload), None);
        }
        assert!(parse_osc("7501;state=blocked:kind=permission").is_none());
        assert!(parse_osc("7501;?").is_none());
    }

    #[test]
    fn all_states_and_state_specific_fields() {
        for (value, state) in [
            ("idle", ProgramState::Idle),
            ("working", ProgramState::Working),
            ("done", ProgramState::Done),
            ("blocked", ProgramState::Blocked),
            ("error", ProgramState::Error),
            ("clear", ProgramState::Clear),
        ] {
            let actual = report(&format!("state={value}:kind=permission:progress=42"));
            assert_eq!(actual.state, state);
            assert_eq!(
                actual.kind,
                (state == ProgramState::Blocked).then_some(BlockedKind::Permission)
            );
            assert_eq!(
                actual.progress,
                matches!(state, ProgramState::Working | ProgramState::Blocked).then_some(42)
            );
        }
        for (value, kind) in [
            ("question", Some(BlockedKind::Question)),
            ("auth", Some(BlockedKind::Auth)),
            ("future", None),
        ] {
            assert_eq!(report(&format!("state=blocked:kind={value}")).kind, kind);
        }
        for bad in ["", "-1", "+1", "101", "256", "1.0", "1e2", "NaN"] {
            assert_eq!(
                report(&format!("state=working:progress={bad}")).progress,
                None
            );
        }
        assert_eq!(report("state=working:progress=0").progress, Some(0));
        assert_eq!(report("state=working:progress=100").progress, Some(100));
        for bad in [
            "app=tool",
            "State=done",
            "state=IDLE",
            "state=future",
            "state=done:state=future",
        ] {
            rejected(bad);
        }
    }

    #[test]
    fn malformed_pairs_unknown_fields_whitespace_and_duplicates() {
        let actual = report("state=idle:state = blocked :missing:=oops:UPPER=bad:future=a/b:msg=$:kind=question:kind=future:progress=99:progress=-1:app=tool:app=bad/name");
        assert_eq!(actual.state, ProgramState::Blocked);
        assert_eq!(actual.message, None);
        assert_eq!(actual.kind, None);
        assert_eq!(actual.progress, None);
        assert_eq!(actual.app, None);
        assert_eq!(
            report("state=done:state=not allowed").state,
            ProgramState::Done
        );
        assert_eq!(
            report("state=done:msg=Zmlyc3Q=:msg=bGFzdA==")
                .message
                .as_deref(),
            Some("last")
        );
    }

    #[test]
    fn invalid_ids_never_address_root_and_paths_are_bounded() {
        for id in [
            "", "/a", "a/", "a//b", "bad,name", "bad=name", "bad name", "a;b", "한글",
        ] {
            rejected(&format!("state=done:id={id}"));
        }
        for id in ["a".repeat(33), "a/".repeat(8) + "a"] {
            rejected(&format!("state=done:id={id}"));
        }
        let id128 = format!(
            "{}/{}/{}/{}",
            "a".repeat(32),
            "b".repeat(32),
            "c".repeat(32),
            "d".repeat(29)
        );
        assert_eq!(id128.len(), 128);
        assert_eq!(
            report(&format!("state=done:id={id128}")).id.as_deref(),
            Some(id128.as_str())
        );
        rejected(&format!("state=done:id={id128}d"));
        assert!(report("state=done:id=A_+.-/b/c/d/e/f/g/h").id.is_some());
        assert_eq!(report("state=done").id, None);
    }

    #[test]
    fn text_is_utf8_single_line_standard_base64_padded_or_unpadded() {
        for text in [
            "",
            "허용할까요? ✅",
            "<b>literal</b>",
            "error permission failed",
        ] {
            for encoded in [
                general_purpose::STANDARD.encode(text),
                general_purpose::STANDARD_NO_PAD.encode(text),
            ] {
                let actual = report(&format!("state=working:msg={encoded}:title={encoded}"));
                assert_eq!(actual.message.as_deref(), Some(text));
                assert_eq!(actual.title.as_deref(), Some(text));
                // Message text has no state meaning and markup is not interpreted.
                assert_eq!(actual.state, ProgramState::Working);
            }
        }
        for code in (0..=31).chain(127..=159) {
            let text = format!("before{}after", char::from_u32(code).unwrap());
            let encoded = general_purpose::STANDARD.encode(text);
            for key in ["title", "msg"] {
                rejected(&format!("state=done:{key}={encoded}"));
            }
        }
        for bad in ["A", "Zg=", "Zg===", "Zg==AAAA", "_w==", "/w==", "Zh=="] {
            rejected(&format!("state=done:msg={bad}"));
        }
        // Even an overridden bad value rejects the entire report.
        rejected("state=done:msg=AA==:msg=b2s=");
        rejected("state=clear:title=/w==:title=b2s=");
    }

    #[test]
    fn every_size_limit_is_checked_before_applying_any_field() {
        let max_message = general_purpose::STANDARD.encode("m".repeat(2048));
        assert_eq!(
            report(&format!("state=done:msg={max_message}"))
                .message
                .unwrap()
                .len(),
            2048
        );
        let too_decoded = general_purpose::STANDARD.encode("m".repeat(2049));
        assert_eq!(too_decoded.len(), 2732);
        rejected(&format!("state=done:msg={too_decoded}"));
        rejected(&format!("state=done:msg={}", "A".repeat(2733)));
        let max_title = general_purpose::STANDARD.encode("t".repeat(192));
        assert_eq!(
            report(&format!("state=done:title={max_title}"))
                .title
                .unwrap()
                .len(),
            192
        );
        rejected(&format!("state=done:title={}", "A".repeat(257)));
        assert_eq!(
            report(&format!("state=done:app={}", "a".repeat(32)))
                .app
                .unwrap()
                .len(),
            32
        );
        rejected(&format!("state=done:app={}:app=ok", "a".repeat(33)));
        rejected(&format!("state=done:{}=value", "k".repeat(17)));
        assert_eq!(
            report(&format!("state=done:{}=value", "k".repeat(16))).state,
            ProgramState::Done
        );
        let prefix = "7501;state=done:future=";
        let max = format!(
            "{prefix}{}",
            "a".repeat(MAX_PROGRAM_STATUS_PAYLOAD - prefix.len())
        );
        assert!(parse_program_status(&max).is_some());
        assert!(parse_program_status(&(max + "a")).is_none());
    }

    #[test]
    fn every_stream_split_and_single_byte_input_preserve_order() {
        let bytes = b"text\x1b]7501;?\x1b\\\x1b]7501;state=working:progress=20\x07\x1b]9;existing notification\x07\x1b]7501;state=blocked:id=child:kind=auth\x1b\\";
        let expected = vec![
            ProgramStatus::Probe,
            ProgramStatus::Report(report("state=working:progress=20")),
            ProgramStatus::Report(report("state=blocked:id=child:kind=auth")),
        ];
        for split in 0..=bytes.len() {
            let mut statuses = vec![];
            let mut notifications = vec![];
            let mut extractor = OscExtractor::new(|payload| {
                statuses.extend(parse_program_status(payload));
                notifications.extend(parse_osc(payload));
            });
            extractor.feed(&bytes[..split]);
            extractor.feed(&bytes[split..]);
            assert_eq!(statuses, expected, "split {split}");
            assert_eq!(notifications.len(), 1);
            assert_eq!(notifications[0].body, "existing notification");
        }
        let mut statuses = vec![];
        let mut extractor =
            OscExtractor::new(|payload| statuses.extend(parse_program_status(payload)));
        for byte in bytes {
            extractor.feed(&[*byte]);
        }
        assert_eq!(statuses, expected);
    }

    #[test]
    fn invalid_or_unterminated_stream_never_partially_changes_records() {
        let mut records = ProgramRecords::default();
        records.apply(report("state=blocked:kind=permission:app=tool"));
        let before = records.records().cloned().collect::<Vec<_>>();
        let mut extractor = OscExtractor::new(|payload| {
            if let Some(ProgramStatus::Report(report)) = parse_program_status(payload) {
                records.apply(report);
            }
        });
        extractor.feed(b"\x1b]7501;state=done:msg=AA==\x07");
        extractor
            .feed(format!("\x1b]7501;state=clear:future={}\x1b\\", "a".repeat(65536)).as_bytes());
        extractor.feed(b"\x1b]7501;state=clear");
        assert_eq!(records.records().cloned().collect::<Vec<_>>(), before);
        let mut extractor = OscExtractor::new(|payload| {
            if let Some(ProgramStatus::Report(report)) = parse_program_status(payload) {
                records.apply(report);
            }
        });
        extractor.feed(b"\x1b]7501;state=error\x07");
        assert_eq!(records.get(None).unwrap().state, ProgramState::Error);
    }

    #[test]
    fn replacement_is_complete_and_app_inherits_from_nearest_existing_ancestor() {
        let mut records = ProgramRecords::default();
        records.apply(report("state=working:app=root"));
        records.apply(report(
            "state=blocked:id=build/test:kind=question:progress=5:title=VGVzdA==",
        ));
        assert_eq!(records.app(Some("build/test")), Some("root"));
        records.apply(report("state=working:id=build:app=builder"));
        assert_eq!(records.app(Some("build/test")), Some("builder"));
        records.apply(report("state=done:id=build/test:app=own"));
        assert_eq!(records.app(Some("build/test")), Some("own"));
        let child = records.get(Some("build/test")).unwrap();
        assert_eq!(
            (child.kind, child.progress, child.title.as_deref()),
            (None, None, None)
        );
        records.apply(report("state=idle:id=build/test"));
        records.apply(report("state=idle:id=build"));
        assert_eq!(records.app(Some("build/test")), Some("root"));
        records.apply(report("state=idle"));
        assert_eq!(records.app(Some("build/test")), None);
    }

    #[test]
    fn clear_removes_exact_subtree_even_without_parent_and_root_clears_all() {
        let mut records = ProgramRecords::default();
        for body in [
            "state=working",
            "state=blocked:id=build/test",
            "state=done:id=builder",
            "state=error:id=build2/test",
            "state=idle:id=other",
        ] {
            records.apply(report(body));
        }
        records.apply(report("state=clear:id=build"));
        assert!(records.get(Some("build/test")).is_none());
        assert!(records.get(Some("builder")).is_some());
        assert!(records.get(Some("build2/test")).is_some());
        assert!(records.get(None).is_some());
        records.apply(report("state=clear:id=missing"));
        assert_eq!(records.records().count(), 4);
        records.apply(report("state=clear"));
        assert_eq!(records.records().count(), 0);
    }

    #[test]
    fn process_boundary_retains_results_and_full_reset_removes_them() {
        let mut records = ProgramRecords::default();
        for state in ["idle", "working", "blocked", "done", "error"] {
            records.apply(report(&format!("state={state}:id={state}")));
        }
        records.process_boundary();
        assert_eq!(
            records.records().map(|r| r.state).collect::<Vec<_>>(),
            vec![ProgramState::Done, ProgramState::Error]
        );
        records.process_boundary();
        assert_eq!(records.records().count(), 2);
        records.reset();
        assert_eq!(records.records().count(), 0);
    }

    #[test]
    fn capacity_evicts_least_recent_update_reads_do_not_refresh_it() {
        let mut records = ProgramRecords::default();
        for index in 0..MAX_PROGRAM_STATUS_RECORDS {
            records.apply(report(&format!("state=working:id=job{index}")));
        }
        records.apply(report("state=blocked:id=job0:kind=permission"));
        assert!(records.get(Some("job1")).is_some());
        records.apply(report("state=done:id=extra"));
        assert_eq!(records.records().count(), MAX_PROGRAM_STATUS_RECORDS);
        assert!(records.get(Some("job1")).is_none());
        assert_eq!(
            records.get(Some("job0")).unwrap().state,
            ProgramState::Blocked
        );
        for _ in 0..1000 {
            records.apply(report("state=done:id=extra"));
        }
        assert_eq!(records.records().count(), MAX_PROGRAM_STATUS_RECORDS);
    }
}
