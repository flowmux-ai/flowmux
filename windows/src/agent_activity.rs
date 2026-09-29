// SPDX-License-Identifier: GPL-3.0-or-later
//! Process-owned native activity. No prompts, tool inputs or transcripts.
use crate::command::SessionHookEvent as Event;
use anyhow::Context;
use flowmux_core::AgentStatus;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    time::{Duration, Instant},
};

const GRACE: Duration = Duration::from_millis(250);
const LIMIT: usize = 256;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Input {
    #[serde(
        default,
        skip_serializing,
        alias = "thread-id",
        alias = "thread_id",
        alias = "sessionID",
        alias = "sessionId",
        alias = "taskId",
        alias = "conversationId"
    )]
    pub(crate) session_id: Option<String>,
    #[serde(default, skip_serializing, deserialize_with = "nonempty_array")]
    pub(crate) background_tasks: bool,
    #[serde(default, skip_serializing, deserialize_with = "nonempty_array")]
    pub(crate) session_crons: bool,
    #[serde(
        default,
        alias = "turn-id",
        alias = "prompt_id",
        skip_serializing_if = "Option::is_none"
    )]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub pending_work: bool,
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
        for id in [
            &self.turn_id,
            &self.agent_id,
            &self.tool_use_id,
            &self.notification_type,
            &self.error,
        ]
        .into_iter()
        .flatten()
        {
            anyhow::ensure!(
                !id.is_empty()
                    && id.len() <= 128
                    && id.bytes().all(
                        |c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b':')
                    ),
                "invalid native activity identity"
            );
        }
        for text in [&self.message, &self.last_assistant_message, &self.tool_name]
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
        if agent == "codex" {
            anyhow::ensure!(
                !matches!(
                    event,
                    Event::ToolStart
                        | Event::ToolEnd
                        | Event::PermissionRequest
                        | Event::ToolBatch
                        | Event::StopFailure
                ),
                "unsupported Codex activity event"
            );
            self.turn_id
                .as_ref()
                .context("Codex activity requires turn_id")?;
        } else {
            anyhow::ensure!(
                agent == "claude" && event != Event::Interrupt,
                "unsupported native activity event/provider"
            );
        }
        if matches!(event, Event::SubagentStart | Event::SubagentStop) {
            self.agent_id
                .as_ref()
                .context("child activity requires agent_id")?;
        }
        anyhow::ensure!(
            !matches!(event, Event::Stop | Event::Interrupt) || self.agent_id.is_none(),
            "root completion cannot carry a child identity"
        );
        Ok(())
    }
}

/// Consume sensitive task/cron entries without retaining their contents.
pub(crate) fn nonempty_array<'de, D: serde::Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    struct Array;
    impl<'de> serde::de::Visitor<'de> for Array {
        type Value = bool;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("an array")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
            let mut nonempty = false;
            while seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                nonempty = true;
            }
            Ok(nonempty)
        }
    }
    d.deserialize_seq(Array)
}

pub enum Activity {
    Codex(Codex),
    Claude(Claude),
}
impl Activity {
    pub fn new(agent: &str) -> Self {
        if agent == "claude" {
            Self::Claude(Claude::default())
        } else {
            Self::Codex(Codex::default())
        }
    }
    pub fn apply(&mut self, event: Event, input: &Input, at: Instant) -> bool {
        match self {
            Self::Codex(s) => s.apply(event, input, at),
            Self::Claude(s) => s.apply(event, input, at),
        }
    }
    pub fn settle(&mut self, now: Instant) -> bool {
        match self {
            Self::Codex(s) => s.settle(now),
            Self::Claude(s) => s.settle(now),
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        match self {
            Self::Codex(s) => s.snapshot(),
            Self::Claude(s) => s.snapshot(),
        }
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
pub struct Codex {
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
impl Codex {
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

#[derive(Default)]
struct ClaudeScope {
    prompt: Option<String>,
    boundary: Option<Instant>,
    last: Option<Instant>,
    waits: BTreeMap<String, (Instant, Option<String>)>,
    text: String,
    idle: bool,
    acknowledged: bool,
    overflow: bool,
}
impl ClaudeScope {
    fn wait(&mut self, key: String, text: Option<String>, at: Instant) -> bool {
        if self.waits.get(&key).is_some_and(|(last, _)| at <= *last) {
            return false;
        }
        // ponytail: 256 wait identities per scope/turn; fail visibly instead
        // of dropping a wait. A new prompt/session resets this ceiling.
        if !self.waits.contains_key(&key) && self.waits.len() >= LIMIT {
            self.overflow = true;
        } else {
            self.waits.insert(key, (at, text));
        }
        true
    }
}

#[derive(Default)]
pub struct Claude {
    root: Option<ClaudeScope>,
    children: BTreeMap<String, ClaudeScope>,
    closed: VecDeque<(Option<String>, Option<String>)>,
    pending: Option<Instant>,
    completion: String,
    overflow: bool,
}
impl Claude {
    fn close(&mut self, key: (Option<String>, Option<String>)) {
        if !self.closed.contains(&key) {
            self.closed.push_back(key);
        }
        // Same bounded replay window as Codex; legacy hooks without prompt_id
        // can only be ordered by receipt, never by an invented producer order.
        if self.closed.len() > LIMIT {
            self.closed.pop_front();
        }
    }
    fn apply(&mut self, event: Event, input: &Input, at: Instant) -> bool {
        let category = input.notification_type.as_deref();
        if event == Event::Notification
            && !matches!(
                category,
                None | Some(
                    "permission_prompt"
                        | "elicitation_dialog"
                        | "elicitation_url_dialog"
                        | "quota_auto_resume_stale"
                        | "quota_auto_resume_fired"
                        | "quota_auto_resume_disabled"
                )
            )
        {
            return false; // Includes background-session notices without a target identity.
        }
        let key = (input.agent_id.clone(), input.turn_id.clone());
        if self.closed.contains(&key) {
            return false;
        }
        if event == Event::SubagentStop {
            let Some(child) = input.agent_id.as_ref() else {
                return false;
            };
            let Some(scope) = self.children.get(child) else {
                return false;
            };
            if scope.last.is_some_and(|last| at <= last)
                || (input.turn_id.is_some()
                    && scope.prompt.is_some()
                    && input.turn_id != scope.prompt)
            {
                return false;
            }
            self.children.remove(child);
            self.close(key);
            if self.pending.is_some() {
                self.pending = Some(at + GRACE);
            }
            return true;
        }
        if event == Event::Stop && self.root.is_none() {
            return false;
        }
        if event == Event::Notification
            && matches!(
                category,
                Some("quota_auto_resume_fired" | "quota_auto_resume_disabled")
            )
        {
            let scope = match &input.agent_id {
                Some(id) => self.children.get(id),
                None => self.root.as_ref(),
            };
            if !scope.is_some_and(|s| s.waits.get("quota").is_some_and(|(_, wait)| wait.is_some()))
            {
                return false;
            }
        }
        if let Some(child) = &input.agent_id {
            if !self.children.contains_key(child) && self.children.len() >= LIMIT {
                self.overflow = true;
                return true;
            }
        }
        let previous = if input.agent_id.is_none() && event == Event::TurnStart {
            self.root.as_ref().and_then(|scope| scope.prompt.clone())
        } else {
            None
        };
        let scope = if let Some(child) = &input.agent_id {
            self.children.entry(child.clone()).or_default()
        } else {
            self.root.get_or_insert_with(Default::default)
        };
        if scope.boundary.is_some_and(|last| at <= last) {
            return false;
        }
        if matches!(event, Event::TurnStart | Event::SubagentStart | Event::Stop)
            && scope.last.is_some_and(|last| at <= last)
        {
            return false;
        }
        if event == Event::TurnStart || event == Event::SubagentStart {
            if input.turn_id.is_some() && input.turn_id == scope.prompt {
                return false;
            }
            *scope = ClaudeScope {
                prompt: input.turn_id.clone(),
                boundary: Some(at),
                ..Default::default()
            };
        } else if input.turn_id.is_some() && scope.prompt.is_some() && input.turn_id != scope.prompt
        {
            return false;
        } else if scope.prompt.is_none() {
            scope.prompt = input.turn_id.clone();
        }
        if scope.idle
            && matches!(event, Event::ToolEnd | Event::ToolBatch | Event::Stop)
            && !input.stop_hook_active
        {
            return false;
        }
        let message = |fallback: &str| {
            input
                .message
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| fallback.into())
        };
        let input_tool = input.tool_name.as_deref().is_some_and(|name| {
            name.eq_ignore_ascii_case("AskUserQuestion")
                || name.eq_ignore_ascii_case("ExitPlanMode")
        });
        let mut text = "Working".to_string();
        let mut idle = false;
        let mut acknowledged = false;
        let accepted = match event {
            Event::TurnStart | Event::SubagentStart | Event::Running => true,
            Event::ToolStart => {
                text = input
                    .tool_name
                    .as_ref()
                    .map_or_else(|| "Working".into(), |name| format!("Using {name}"));
                !input_tool
                    || scope.wait(
                        input
                            .tool_use_id
                            .as_ref()
                            .map_or_else(|| "tool:unknown".into(), |id| format!("tool:{id}")),
                        Some(message("Waiting for input")),
                        at,
                    )
            }
            Event::ToolEnd => {
                // Only a matching call resolves a question; a parallel tool,
                // failure or denial must not clear a different waiting call.
                !input_tool
                    || input
                        .tool_use_id
                        .as_ref()
                        .is_none_or(|id| scope.wait(format!("tool:{id}"), None, at))
            }
            Event::PermissionRequest => scope.wait(
                "permission".into(),
                Some(message("Waiting for approval")),
                at,
            ),
            Event::ToolBatch => scope.wait("permission".into(), None, at),
            Event::Notification => match category {
                Some("permission_prompt" | "elicitation_dialog" | "elicitation_url_dialog") => {
                    scope.wait("permission".into(), Some(message("Waiting for input")), at)
                }
                Some("quota_auto_resume_stale") => {
                    scope.wait("quota".into(), Some(message("Waiting for input")), at)
                }
                Some("quota_auto_resume_fired" | "quota_auto_resume_disabled") => {
                    if !scope
                        .waits
                        .get("quota")
                        .is_some_and(|(_, wait)| wait.is_some())
                    {
                        return false;
                    }
                    idle = category == Some("quota_auto_resume_disabled");
                    acknowledged = idle;
                    if idle {
                        text = "Auto-resume disabled".into();
                    }
                    scope.wait("quota".into(), None, at)
                }
                _ => scope.wait("input".into(), Some(message("Waiting for input")), at),
            },
            Event::StopFailure => {
                let kind = if matches!(input.error.as_deref(), Some("rate_limit" | "usage_limit")) {
                    "quota"
                } else {
                    "api_failure"
                };
                scope.wait(
                    kind.into(),
                    Some(
                        input
                            .last_assistant_message
                            .clone()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| {
                                input.error.as_ref().map_or_else(
                                    || "API request failed".into(),
                                    |error| format!("API error: {error}"),
                                )
                            }),
                    ),
                    at,
                )
            }
            Event::Stop => {
                scope.waits.clear();
                scope.boundary = Some(at);
                text = if input.pending_work {
                    "Background work pending"
                } else {
                    "Working"
                }
                .into();
                self.completion = input
                    .last_assistant_message
                    .clone()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "Done".into());
                true
            }
            _ => false,
        };
        if !accepted {
            return false;
        }
        scope.text = text;
        scope.last = Some(scope.last.map_or(at, |last| last.max(at)));
        scope.idle = idle;
        scope.acknowledged = acknowledged;
        if input.agent_id.is_none() {
            self.pending = (event == Event::Stop && !input.pending_work).then_some(at + GRACE);
            if let Some(previous) = previous.filter(|id| Some(id) != input.turn_id.as_ref()) {
                self.close((None, Some(previous)));
            }
        }
        true
    }
    fn settle(&mut self, now: Instant) -> bool {
        if self.children.is_empty() && self.pending.is_some_and(|due| now >= due) && !self.overflow
        {
            self.pending = None;
            if let Some(root) = &mut self.root {
                if root.overflow {
                    return false;
                }
                root.idle = true;
                root.text = self.completion.clone();
                return true;
            }
        }
        false
    }
    fn snapshot(&self) -> Snapshot {
        let scopes = || self.root.iter().chain(self.children.values());
        let (status, text) = if self.overflow || scopes().any(|s| s.overflow) {
            (AgentStatus::Unknown, "Native activity limit reached".into())
        } else if let Some((_, Some(text))) = scopes()
            .flat_map(|s| s.waits.values())
            .filter(|(_, text)| text.is_some())
            .max_by_key(|(at, _)| *at)
        {
            (AgentStatus::Blocked, text.clone())
        } else if !self.children.is_empty() {
            (
                AgentStatus::Working,
                format!("{} active Claude subagent(s)", self.children.len()),
            )
        } else if let Some(root) = &self.root {
            (
                if root.idle {
                    AgentStatus::Idle
                } else {
                    AgentStatus::Working
                },
                root.text.clone(),
            )
        } else {
            (AgentStatus::Unknown, "Ready".into())
        };
        Snapshot {
            status,
            text,
            interrupted: status == AgentStatus::Idle
                && self.root.as_ref().is_some_and(|root| root.acknowledged),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claude_waits_are_scoped_and_background_or_failed_turns_do_not_complete() {
        let now = Instant::now();
        let mut tick = 0;
        let mut state = Claude::default();
        let mut send = |state: &mut Claude, event, input: &Input| {
            tick += 1;
            state.apply(event, input, now + Duration::from_millis(tick))
        };
        let mut input = Input {
            turn_id: Some("prompt-a".into()),
            message: Some("한 한 é 😀 승인".into()),
            ..Default::default()
        };
        assert!(send(&mut state, Event::TurnStart, &input));
        send(&mut state, Event::PermissionRequest, &input);
        send(&mut state, Event::ToolEnd, &input);
        assert_eq!(state.snapshot().status, AgentStatus::Blocked);
        input.tool_name = Some("AskUserQuestion".into());
        input.tool_use_id = Some("question-a".into());
        send(&mut state, Event::ToolStart, &input);
        send(&mut state, Event::ToolBatch, &input);
        assert_eq!(state.snapshot().text, "한 한 é 😀 승인");
        input.tool_use_id = Some("question-b".into());
        send(&mut state, Event::ToolEnd, &input);
        assert_eq!(state.snapshot().status, AgentStatus::Blocked);
        input.tool_use_id = Some("question-a".into());
        send(&mut state, Event::ToolEnd, &input);
        assert_eq!(state.snapshot().status, AgentStatus::Working);
        input.agent_id = Some("child".into());
        send(&mut state, Event::SubagentStart, &input);
        send(&mut state, Event::PermissionRequest, &input);
        input.agent_id = None;
        input.last_assistant_message = Some("한글 완료".into());
        send(&mut state, Event::Stop, &input);
        assert!(!state.settle(now + Duration::from_secs(1)));
        assert_eq!(state.snapshot().status, AgentStatus::Blocked);
        input.agent_id = Some("child".into());
        send(&mut state, Event::SubagentStop, &input);
        assert!(!send(&mut state, Event::PermissionRequest, &input));
        assert!(state.settle(now + Duration::from_secs(1)));
        assert_eq!(state.snapshot().text, "한글 완료");
        input.agent_id = None;
        assert!(!send(&mut state, Event::Stop, &input));
        input.turn_id = Some("prompt-b".into());
        send(&mut state, Event::TurnStart, &input);
        input.turn_id = Some("prompt-a".into());
        assert!(!send(&mut state, Event::Stop, &input));
        input.turn_id = Some("prompt-b".into());
        input.pending_work = true;
        send(&mut state, Event::Stop, &input);
        assert!(!state.settle(now + Duration::from_secs(1)));
        assert_eq!(state.snapshot().text, "Background work pending");
        input.last_assistant_message = None;
        input.error = Some("rate_limit".into());
        send(&mut state, Event::StopFailure, &input);
        send(&mut state, Event::ToolBatch, &input);
        assert_eq!(state.snapshot().text, "API error: rate_limit");
        input.notification_type = Some("quota_auto_resume_disabled".into());
        send(&mut state, Event::Notification, &input);
        assert_eq!(state.snapshot().status, AgentStatus::Idle);
        assert!(state.snapshot().interrupted);
        input.notification_type = Some("agent_completed".into());
        assert!(!send(&mut state, Event::Notification, &input));
        // A delayed earlier call cannot reopen a wait already resolved by its ID.
        let mut scope = ClaudeScope::default();
        scope.wait("tool:a".into(), None, now + Duration::from_millis(2));
        assert!(!scope.wait("tool:a".into(), Some("Wait".into()), now));
        assert!(scope.wait("tool:b".into(), Some("Wait".into()), now));
        let mut reordered = Claude::default();
        input.agent_id = Some("child".into());
        reordered.apply(Event::SubagentStart, &input, now);
        reordered.apply(
            Event::PermissionRequest,
            &input,
            now + Duration::from_millis(2),
        );
        assert!(!reordered.apply(Event::SubagentStop, &input, now + Duration::from_millis(1)));
        assert_eq!(reordered.snapshot().status, AgentStatus::Blocked);
    }
    #[test]
    fn scoped_turns_waits_children_and_late_events_do_not_invent_completion() {
        let start = Instant::now();
        let mut clock = start;
        let mut state = Codex::default();
        let mut send = |state: &mut Codex, event, turn: &str, child: Option<&str>| {
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
