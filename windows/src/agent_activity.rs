// SPDX-License-Identifier: GPL-3.0-or-later
//! Process-owned Codex turn/child state. No prompts, tool inputs or transcripts.
use crate::command::SessionHookEvent as Event;
use anyhow::Context;
use flowmux_core::AgentStatus;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

pub const MAX_HOOK_BYTES: usize = 1024 * 1024;
const GRACE: Duration = Duration::from_millis(250);
const LIMIT: usize = 256;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Input {
    #[serde(default, alias = "turn-id", skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub stop_hook_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(
        default,
        alias = "last-assistant-message",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_assistant_message: Option<String>,
}
impl Input {
    pub fn normalize(&mut self) {
        for text in [&mut self.message, &mut self.last_assistant_message]
            .into_iter()
            .flatten()
        {
            *text = text.split_whitespace().collect::<Vec<_>>().join(" ");
            text.retain(|c| !c.is_control());
            if text.len() > 1024 {
                let end = text
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|i| *i <= 1021)
                    .last()
                    .unwrap_or(0);
                text.truncate(end);
                text.push('…');
            }
        }
    }
    pub fn validate(&self, event: Event, agent: &str) -> anyhow::Result<()> {
        for id in [&self.turn_id, &self.agent_id].into_iter().flatten() {
            anyhow::ensure!(
                !id.is_empty()
                    && id.len() <= 128
                    && id.bytes().all(
                        |c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b':')
                    ),
                "invalid native activity identity"
            );
        }
        for text in [&self.message, &self.last_assistant_message]
            .into_iter()
            .flatten()
        {
            anyhow::ensure!(
                text.len() <= 1024 && !text.chars().any(char::is_control),
                "invalid native activity text"
            );
        }
        if event.is_session() {
            return Ok(());
        }
        anyhow::ensure!(
            agent == "codex",
            "native activity hooks are currently supported for Codex"
        );
        self.turn_id
            .as_ref()
            .context("Codex activity requires turn_id")?;
        if matches!(event, Event::SubagentStart | Event::SubagentStop) {
            self.agent_id
                .as_ref()
                .context("Codex child activity requires agent_id")?;
        }
        anyhow::ensure!(
            !matches!(event, Event::Stop | Event::Interrupt) || self.agent_id.is_none(),
            "root completion cannot carry a child identity"
        );
        Ok(())
    }
}

struct Turn {
    id: String,
    last: Instant,
    blocked: Option<String>,
}
impl Turn {
    fn new(id: &str, last: Instant) -> Self {
        Self {
            id: id.into(),
            last,
            blocked: None,
        }
    }
}
#[derive(Default)]
pub struct Activity {
    root: Option<Turn>,
    children: BTreeMap<String, Turn>,
    closed: VecDeque<(Option<String>, String)>,
    pending: Option<Instant>,
    settled: bool,
    interrupted: bool,
    completion: Option<String>,
    overflow: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub status: AgentStatus,
    pub text: String,
    pub interrupted: bool,
}
impl Activity {
    fn close(&mut self, child: Option<String>, turn: String) {
        let key = (child, turn);
        if !self.closed.contains(&key) {
            self.closed.push_back(key);
        }
        // ponytail: remember 256 completed identities; older replay protection
        // needs a provider-issued sequence, which native hooks do not supply.
        if self.closed.len() > LIMIT {
            self.closed.pop_front();
        }
    }
    pub fn apply(&mut self, event: Event, input: &Input, received: Instant) -> bool {
        let Some(id) = input.turn_id.as_deref() else {
            return false;
        };
        let key = (input.agent_id.clone(), id.to_owned());
        if self.closed.contains(&key) {
            if event != Event::Stop
                || !input.stop_hook_active
                || self.root.as_ref().is_none_or(|turn| turn.id != id)
            {
                return false;
            }
            self.closed.retain(|item| item != &key);
        }
        if let Some(child) = &input.agent_id {
            if self
                .children
                .get(child)
                .is_some_and(|turn| received <= turn.last)
            {
                return false;
            }
            if event == Event::SubagentStop {
                if self.children.get(child).is_some_and(|turn| turn.id == id) {
                    self.children.remove(child);
                }
                self.close(Some(child.clone()), id.into());
                if self.pending.is_some() {
                    self.pending = Some(received + GRACE);
                }
                return true;
            }
            if !self.children.contains_key(child) && self.children.len() >= LIMIT {
                self.overflow = true;
                return true;
            }
            if self.children.get(child).is_some_and(|turn| turn.id != id) {
                let previous = self.children.remove(child).unwrap();
                self.close(Some(child.clone()), previous.id);
            }
            let turn = self
                .children
                .entry(child.clone())
                .or_insert_with(|| Turn::new(id, received));
            turn.last = received;
            if event == Event::Notification {
                turn.blocked = Some(
                    input
                        .message
                        .clone()
                        .unwrap_or_else(|| "Waiting for input".into()),
                );
            }
            // Ordinary progress cannot identify which parallel permission
            // request resolved. The child boundary owns clearing that wait.
            return true;
        }
        if self.root.as_ref().is_some_and(|turn| received <= turn.last) {
            return false;
        }
        if event == Event::TurnStart {
            if self.root.as_ref().is_some_and(|turn| turn.id == id) {
                return false;
            }
            if let Some(previous) = self.root.take() {
                self.close(None, previous.id);
            }
            self.root = Some(Turn::new(id, received));
            self.pending = None;
            self.settled = false;
            self.interrupted = false;
            self.completion = None;
            return true;
        }
        if self.root.is_none() && matches!(event, Event::Running | Event::Notification) {
            self.root = Some(Turn::new(id, received));
        }
        let Some(turn) = &mut self.root else {
            return false;
        };
        if turn.id != id {
            return false;
        }
        turn.last = received;
        match event {
            Event::Running => {
                self.pending = None;
                self.settled = false;
            }
            Event::Notification => {
                turn.blocked = Some(
                    input
                        .message
                        .clone()
                        .unwrap_or_else(|| "Waiting for input".into()),
                );
                self.pending = None;
                self.settled = false;
            }
            Event::Stop | Event::Interrupt => {
                turn.blocked = None;
                self.pending = Some(received + GRACE);
                self.settled = false;
                self.interrupted = event == Event::Interrupt;
                self.completion = input.last_assistant_message.clone();
            }
            _ => return false,
        }
        true
    }
    pub fn settle(&mut self, now: Instant) -> bool {
        if !self.overflow && self.children.is_empty() && self.pending.is_some_and(|due| now >= due)
        {
            self.pending = None;
            self.settled = true;
            if let Some(root) = &self.root {
                self.close(None, root.id.clone());
            }
            true
        } else {
            false
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        let (status, text) = if self.overflow {
            (AgentStatus::Unknown, "Native activity limit reached".into())
        } else if let Some(wait) = self
            .root
            .iter()
            .chain(self.children.values())
            .filter(|turn| turn.blocked.is_some())
            .max_by_key(|turn| turn.last)
        {
            (AgentStatus::Blocked, wait.blocked.clone().unwrap())
        } else if !self.children.is_empty() {
            (
                AgentStatus::Working,
                format!("{} active Codex subagent(s)", self.children.len()),
            )
        } else if self.settled {
            (
                AgentStatus::Idle,
                if self.interrupted {
                    "Turn interrupted".into()
                } else {
                    self.completion.clone().unwrap_or_else(|| "Done".into())
                },
            )
        } else if self.root.is_none() {
            (AgentStatus::Unknown, "Ready".into())
        } else {
            (AgentStatus::Working, "Working".into())
        };
        Snapshot {
            status,
            text,
            interrupted: self.interrupted && self.settled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_turns_waits_children_and_late_events_do_not_invent_completion() {
        let start = Instant::now();
        let mut clock = start;
        let mut state = Activity::default();
        let mut send = |state: &mut Activity, event, turn: &str, child: Option<&str>| {
            clock += Duration::from_millis(1);
            state.apply(
                event,
                &Input {
                    turn_id: Some(turn.into()),
                    agent_id: child.map(str::to_string),
                    message: Some("한 한 é 😀 입력".into()),
                    ..Input::default()
                },
                clock,
            )
        };
        assert!(!send(&mut state, Event::Stop, "unseen", None));
        assert!(send(&mut state, Event::TurnStart, "a", None));
        send(&mut state, Event::Notification, "a", None);
        send(&mut state, Event::Running, "a", None);
        assert_eq!(state.snapshot().status, AgentStatus::Blocked);
        assert_eq!(state.snapshot().text, "한 한 é 😀 입력");
        send(&mut state, Event::SubagentStart, "child-a", Some("child"));
        send(&mut state, Event::Stop, "a", None);
        assert!(!state.settle(start + Duration::from_secs(1)));
        assert_eq!(state.snapshot().status, AgentStatus::Working);
        send(&mut state, Event::Notification, "child-a", Some("child"));
        send(&mut state, Event::Running, "child-a", Some("child"));
        assert_eq!(state.snapshot().status, AgentStatus::Blocked);
        send(&mut state, Event::SubagentStop, "child-a", Some("child"));
        assert!(!send(
            &mut state,
            Event::SubagentStart,
            "child-a",
            Some("child")
        ));
        assert!(state.settle(start + Duration::from_secs(1)));
        assert_eq!(state.snapshot().status, AgentStatus::Idle);
        assert!(!send(&mut state, Event::Stop, "a", None));
        assert!(!send(&mut state, Event::Running, "a", None));
        send(&mut state, Event::TurnStart, "b", None);
        assert!(!send(&mut state, Event::Stop, "a", None));
        send(&mut state, Event::Interrupt, "b", None);
        state.settle(start + Duration::from_secs(1));
        assert!(state.snapshot().interrupted);
        send(&mut state, Event::TurnStart, "c", None);
        send(&mut state, Event::Stop, "c", None);
        send(&mut state, Event::Running, "c", None);
        assert!(!state.settle(start + Duration::from_secs(1)));
        assert_eq!(state.snapshot().status, AgentStatus::Working);
    }
}
