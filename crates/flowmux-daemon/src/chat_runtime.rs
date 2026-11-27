// SPDX-License-Identifier: GPL-3.0-or-later

mod acp;
mod claude;
mod codex;
mod commands;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    Codex,
    Claude,
    OpenCode,
    Gemini,
    Cline,
    Antigravity,
}

impl Provider {
    pub const ALL: [Self; 6] = [
        Self::Codex,
        Self::Claude,
        Self::OpenCode,
        Self::Gemini,
        Self::Cline,
        Self::Antigravity,
    ];

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|agent| agent.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::OpenCode => "OpenCode",
            Self::Gemini => "Gemini",
            Self::Cline => "Cline",
            Self::Antigravity => "Antigravity",
        }
    }

    fn args(self) -> &'static [&'static str] {
        match self {
            Self::Codex => &["app-server"],
            Self::Claude => claude::ARGS,
            Self::OpenCode | Self::Antigravity => &["acp"],
            Self::Gemini | Self::Cline => &["--acp"],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub description: String,
}

impl Choice {
    fn new(id: &str, label: &str, description: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: description.into(),
        }
    }

    fn elevated(&self) -> bool {
        let id = self.id.to_ascii_lowercase().replace(['-', '_', ' '], "");
        let label = self.label.to_ascii_lowercase().replace(['-', '_', ' '], "");
        [id, label].into_iter().any(|value| {
            [
                "fullaccess",
                "bypasspermissions",
                "yolo",
                "approveall",
                "allowall",
                "unrestricted",
            ]
            .iter()
            .any(|needle| value.contains(needle))
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Menu {
    pub choices: Vec<Choice>,
    pub current: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Catalog {
    pub models: Menu,
    pub access: Menu,
    pub efforts: Menu,
    pub model_efforts: HashMap<String, Menu>,
    pub plan: bool,
    /// Slash commands and skills the agent accepts in its own syntax.
    #[serde(default)]
    pub commands: Vec<SlashCommand>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Arguments the command takes, such as `[instructions]`.
    #[serde(default)]
    pub hint: String,
    #[serde(default)]
    pub skill: bool,
    /// Codex invokes a skill with an input item naming this file.
    #[serde(default)]
    pub path: Option<String>,
}

const MAX_COMMANDS: usize = 400;

impl SlashCommand {
    fn new(name: &str, description: &str, hint: &str) -> Self {
        Self {
            name: name.into(),
            // Skill descriptions can run to paragraphs; the menu shows one line.
            description: description
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim()
                .chars()
                .take(240)
                .collect(),
            hint: hint.chars().take(80).collect(),
            skill: false,
            path: None,
        }
    }
}

impl Catalog {
    pub fn efforts_for(&self, model: Option<&str>) -> &Menu {
        model
            .and_then(|model| self.model_efforts.get(model))
            .unwrap_or(&self.efforts)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    pub model: Option<String>,
    pub access: Option<String>,
    pub effort: Option<String>,
    pub plan: bool,
    pub confirmed_access: bool,
}

pub use flowmux_state::chat_history::{Change, Prompt, Question, TextKind, Work};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    Attached {
        records: Vec<flowmux_state::chat_history::Record>,
        busy: bool,
        native_id: Option<String>,
        settings: Settings,
    },
    Detached,
    Ready(Catalog),
    Session(String),
    SettingsApplied(Settings),
    Text {
        id: String,
        kind: TextKind,
        text: String,
        append: bool,
    },
    Work(Work),
    Prompt(Prompt),
    PromptResolved {
        id: String,
        label: String,
        answers: Vec<String>,
    },
    Finished {
        status: String,
        detail: Option<String>,
    },
    Rejected(String),
    /// A queued message joined the running turn.
    Steered {
        text: String,
    },
    SteerRejected(String),
    Notice(String),
    StopFailed(String),
    Offline(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Input {
    End,
    Send {
        text: String,
        settings: Settings,
    },
    Stop,
    /// Adds a message to the running turn when the provider supports it.
    Steer {
        text: String,
    },
    Respond {
        id: String,
        selected: usize,
        answers: Vec<String>,
    },
}

const MAX_FRAME: u64 = 8 * 1024 * 1024;
const MAX_DETAIL: usize = 32 * 1024;

struct Wire {
    child: Child,
    stdout: BufReader<ChildStdout>,
    stdin: ChildStdin,
    next_id: u64,
    read_buffer: Vec<u8>,
    /// ACP can report its commands while a request is pending; `rpc` keeps the latest list.
    commands: Option<Value>,
}

impl Wire {
    fn start(
        provider: Provider,
        program: &Path,
        cwd: &Path,
        native_id: Option<&str>,
        environment: &[(String, String)],
    ) -> Result<Self> {
        let mut command = Command::new(program);
        command.args(provider.args());
        if let Some(id) = native_id.filter(|_| provider == Provider::Claude) {
            let id = uuid::Uuid::parse_str(id).context("Invalid Claude session ID")?;
            command.args(["--resume", &id.to_string()]);
        }
        let mut child = command
            .current_dir(cwd)
            .env_clear()
            .envs(environment.iter().map(|(key, value)| (key, value)))
            .env("FLOWMUX_CHAT_SESSION", "1")
            .env_remove("CLAUDECODE")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("CODEX_SESSION_ID")
            .env_remove("FLOWMUX_PANE_ID")
            .env_remove("FLOWMUX_SURFACE_ID")
            .env_remove("FLOWMUX_WORKSPACE_ID")
            .env_remove("FLOWMUX_TAB_ID")
            .env_remove("FLOWMUX_SOCKET_PATH")
            .env_remove("FLOWMUX_TEAM_JOB")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("Could not start {}", provider.name()))?;
        let stdout = BufReader::new(child.stdout.take().context("Missing agent stdout")?);
        let stdin = child.stdin.take().context("Missing agent stdin")?;
        Ok(Self {
            child,
            stdout,
            stdin,
            next_id: 0,
            read_buffer: Vec::new(),
            commands: None,
        })
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    async fn read(&mut self) -> Result<Option<Value>> {
        loop {
            let remaining = MAX_FRAME + 1 - self.read_buffer.len() as u64;
            let len = (&mut self.stdout)
                .take(remaining)
                .read_until(b'\n', &mut self.read_buffer)
                .await?;
            if len == 0 && self.read_buffer.is_empty() {
                return Ok(None);
            }
            if self.read_buffer.len() as u64 > MAX_FRAME {
                bail!("The agent sent an oversized message.");
            }
            let value = serde_json::from_slice::<Value>(&self.read_buffer);
            self.read_buffer.clear();
            if let Ok(value) = value {
                if value.is_object() {
                    return Ok(Some(value));
                }
            }
        }
    }

    async fn write(&mut self, value: Value) -> Result<()> {
        let mut line = serde_json::to_vec(&value)?;
        line.push(b'\n');
        tokio::time::timeout(Duration::from_secs(10), self.stdin.write_all(&line))
            .await
            .context("Writing to the agent timed out")??;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.id();
        self.write(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await?;
        Ok(id)
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.request(method, params).await?;
        tokio::time::timeout(Duration::from_secs(20), async {
            while let Some(frame) = self.read().await? {
                if frame["id"].as_u64() == Some(id) && frame["method"].is_null() {
                    return response(&frame);
                }
                if frame["params"]["update"]["sessionUpdate"] == "available_commands_update" {
                    self.commands = Some(frame["params"]["update"].clone());
                }
                if frame.get("method").is_some() && frame.get("id").is_some() {
                    self.unsupported(&frame["id"]).await?;
                }
            }
            bail!("The agent exited during {method}.")
        })
        .await
        .with_context(|| format!("The agent did not respond to {method}"))?
    }

    async fn unsupported(&mut self, id: &Value) -> Result<()> {
        self.write(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Request unavailable in Flowmux Chat"}})).await
    }
}

impl Drop for Wire {
    fn drop(&mut self) {
        if let Some(pid) = self.child.id() {
            // Every chat starts its own process group, including CLI helpers.
            unsafe {
                libc::killpg(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

enum Reply {
    Rpc(Vec<Value>),
    CodexQuestions(Vec<String>),
    Acp(Vec<String>),
    Claude(Value),
}

struct Pending {
    wire_id: Value,
    labels: Vec<String>,
    reply: Reply,
}

#[derive(Default)]
struct Turn {
    request: Option<u64>,
    interrupt_request: Option<u64>,
    remote_id: Option<String>,
    stop: bool,
    stop_deadline: Option<tokio::time::Instant>,
    pending: HashMap<String, Pending>,
    work: HashMap<String, Work>,
    settings: Settings,
    message_id: String,
    acp_text: Option<(TextKind, u64)>,
    acp_next_text: u64,
    steer: Option<(u64, String)>,
    /// A Codex `/compact` turn, which may end with `thread/compacted` alone.
    compact: bool,
    /// A Codex review can run in its own thread.
    review_thread: Option<String>,
}

struct Session {
    key: String,
    catalog: Catalog,
    active: Option<Turn>,
    model_option: Option<String>,
    access_option: Option<String>,
    effort_option: Option<String>,
    /// A steer whose turn ended before Codex answered; the answer still decides its fate.
    late_steer: Option<(u64, String)>,
}

impl Session {
    fn new() -> Self {
        Self {
            key: String::new(),
            catalog: Catalog::default(),
            active: None,
            model_option: None,
            access_option: None,
            effort_option: None,
            late_steer: None,
        }
    }

    fn accepts_settings(&self, settings: &Settings) -> bool {
        let model = valid_choice(&self.catalog.models, settings.model.as_deref());
        let access = valid_choice(&self.catalog.access, settings.access.as_deref());
        let efforts = self.catalog.efforts_for(settings.model.as_deref());
        model
            && access
            && valid_choice(efforts, settings.effort.as_deref())
            && (!settings.plan || self.catalog.plan)
            && (settings.confirmed_access
                || !settings.access.as_ref().is_some_and(|id| {
                    self.catalog.access.choices.iter().any(|choice| {
                        &choice.id == id
                            && choice.elevated()
                            && self.catalog.access.current.as_ref() != Some(id)
                    })
                }))
    }
}

fn valid_choice(menu: &Menu, id: Option<&str>) -> bool {
    id.is_none_or(|id| menu.choices.iter().any(|choice| choice.id == id))
}

pub fn starts_with_elevated_access(choice: &Choice) -> bool {
    choice.elevated()
}

pub fn start(
    runtime: &tokio::runtime::Handle,
    provider: Provider,
    program: PathBuf,
    cwd: PathBuf,
    environment: Vec<(String, String)>,
    native_id: Option<String>,
) -> (Sender<Input>, Receiver<Event>, tokio::task::JoinHandle<()>) {
    let (tx, input) = async_channel::bounded(16);
    let (output, rx) = async_channel::bounded(128);
    let task = runtime.spawn(async move {
        let result = tokio::select! {
            result = run(provider, &program, &cwd, native_id.as_deref(), &environment, &input, &output) => result,
            () = output.closed() => Ok(()),
        };
        if let Err(error) = result {
            let _ = output
                .send(Event::Offline(clip(&format!("{error:#}"), 2048)))
                .await;
        }
    });
    (tx, rx, task)
}

async fn run(
    provider: Provider,
    program: &Path,
    cwd: &Path,
    native_id: Option<&str>,
    environment: &[(String, String)],
    input: &Receiver<Input>,
    events: &Sender<Event>,
) -> Result<()> {
    if !cwd.is_dir() {
        bail!("The working directory is unavailable.");
    }
    if cwd.to_str().is_none() {
        bail!("Chat requires a UTF-8 working directory.");
    }
    let mut wire = Wire::start(provider, program, cwd, native_id, environment)?;
    let mut session = Session::new();
    match provider {
        Provider::Codex => codex::boot(&mut wire, &mut session, cwd, native_id).await?,
        Provider::Claude => {
            claude::boot(&mut wire, &mut session).await?;
            session.key = native_id.unwrap_or("").into();
        }
        _ => acp::boot(&mut wire, &mut session, cwd, native_id).await?,
    }
    emit(events, Event::Ready(session.catalog.clone())).await?;
    let mut has_sent = false;
    let mut reported_id = String::new();
    loop {
        let stop_deadline = session.active.as_ref().and_then(|turn| turn.stop_deadline);
        tokio::select! {
            () = async {
                match stop_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => bail!("The agent did not stop in time. Its chat connection was closed."),
            frame = wire.read() => {
                let Some(frame) = frame? else { bail!("The agent session ended."); };
                match provider {
                    Provider::Codex => codex::frame(&mut wire, &mut session, events, &frame).await?,
                    Provider::Claude => claude::frame(&mut wire, &mut session, events, &frame).await?,
                    _ => acp::frame(&mut wire, &mut session, events, &frame).await?,
                }
            }
            next = input.recv() => {
                match next {
                    Err(_) | Ok(Input::End) => return Ok(()),
                    Ok(Input::Send { text, settings }) if session.active.is_none() => {
                        if !session.accepts_settings(&settings) {
                            emit(events, Event::Rejected("The selected chat settings are unavailable. Review them and try again.".into())).await?;
                            continue;
                        }
                        let result = match provider {
                            Provider::Codex => codex::send(&mut wire, &session, cwd, &text, &settings).await.map(Some),
                            Provider::Claude => claude::send(&mut wire, &mut session, &text, &settings).await.map(|()| None),
                            _ => acp::send(&mut wire, &mut session, &text, &settings).await.map(Some),
                        };
                        match result {
                            Ok(request) => {
                                if provider != Provider::Codex {
                                    emit(events, Event::SettingsApplied(settings.clone())).await?;
                                }
                                session.active = Some(Turn {
                                    request,
                                    settings,
                                    compact: provider == Provider::Codex
                                        && commands::codex(&text).is_some_and(|(run, _)| run == commands::Run::Compact),
                                    ..Turn::default()
                                });
                                has_sent = true;
                            }
                            Err(error) => {
                                emit(events, Event::Rejected(clip(&format!("{error:#}"), 2048))).await?;
                            }
                        }
                    }
                    Ok(Input::Send { .. }) => {}
                    Ok(Input::Stop) => {
                        if let Some(turn) = session.active.as_mut().filter(|turn| !turn.stop) {
                            turn.stop = true;
                            turn.stop_deadline = Some(tokio::time::Instant::now() + Duration::from_secs(12));
                            match provider {
                                Provider::Codex => codex::interrupt(&mut wire, &session.key, turn).await?,
                                Provider::Claude => claude::interrupt(&mut wire).await?,
                                _ => wire.notify("session/cancel", json!({"sessionId":session.key})).await?,
                            }
                        }
                    }
                    Ok(Input::Steer { text }) => {
                        let turn = session.active.as_mut().filter(|turn| {
                            provider == Provider::Codex
                                && !turn.stop
                                && turn.steer.is_none()
                                && turn.remote_id.is_some()
                        });
                        if let Some(turn) = turn {
                            codex::steer(&mut wire, &session.key, turn, text).await?;
                        } else {
                            emit(events, Event::SteerRejected("The running turn cannot take another message now.".into())).await?;
                        }
                    }
                    Ok(Input::Respond { id, selected, answers }) => {
                        if let Some(turn) = session.active.as_mut().filter(|turn| !turn.stop) {
                            respond(&mut wire, turn, events, id, selected, answers).await?;
                        }
                    }
                }
            }
        }
        if has_sent && !session.key.is_empty() && session.key != reported_id {
            reported_id.clone_from(&session.key);
            emit(events, Event::Session(reported_id.clone())).await?;
        }
    }
}

async fn respond(
    wire: &mut Wire,
    turn: &mut Turn,
    events: &Sender<Event>,
    id: String,
    selected: usize,
    answers: Vec<String>,
) -> Result<()> {
    let Some(pending) = turn.pending.get(&id) else {
        return Ok(());
    };
    let Some(label) = pending.labels.get(selected) else {
        return Ok(());
    };
    let result = match &pending.reply {
        Reply::Rpc(choices) => choices[selected].clone(),
        Reply::Acp(ids) => json!({"outcome":{"outcome":"selected","optionId":ids[selected]}}),
        Reply::CodexQuestions(ids)
            if ids.len() == answers.len()
                && answers.iter().all(|value| !value.trim().is_empty()) =>
        {
            let answers: serde_json::Map<_, _> = ids
                .iter()
                .cloned()
                .zip(answers.iter().map(|answer| json!({"answers":[answer]})))
                .collect();
            json!({"answers":answers})
        }
        Reply::CodexQuestions(_) => {
            emit(
                events,
                Event::Notice("Answer every question before sending.".into()),
            )
            .await?;
            return Ok(());
        }
        Reply::Claude(input) => {
            let response = if selected == 0 {
                json!({"behavior":"allow","updatedInput":input})
            } else {
                json!({"behavior":"deny","message":"The user declined the request."})
            };
            wire.write(json!({"type":"control_response","response":{"subtype":"success","request_id":pending.wire_id,"response":response}})).await?;
            let label = label.clone();
            turn.pending.remove(&id);
            return emit(events, Event::PromptResolved { id, label, answers }).await;
        }
    };
    wire.write(json!({"jsonrpc":"2.0","id":pending.wire_id,"result":result}))
        .await?;
    let label = label.clone();
    turn.pending.remove(&id);
    emit(events, Event::PromptResolved { id, label, answers }).await
}

async fn emit(events: &Sender<Event>, event: Event) -> Result<()> {
    events.send(event).await.map_err(|_| anyhow!("Chat closed"))
}

fn response(frame: &Value) -> Result<Value> {
    if let Some(error) = frame.get("error") {
        bail!(
            "{}",
            clip(
                error["message"]
                    .as_str()
                    .unwrap_or("The agent rejected the request."),
                2048
            )
        );
    }
    frame
        .get("result")
        .cloned()
        .context("Missing agent response")
}

fn values(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

fn str_at<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

fn clip(value: &str, max: usize) -> String {
    if value.len() <= max {
        return value.into();
    }
    let mut end = max;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n… (truncated)", &value[..end])
}

async fn settle(
    session: &mut Session,
    events: &Sender<Event>,
    status: &str,
    detail: Option<String>,
) -> Result<()> {
    if let Some(turn) = session.active.take() {
        session.late_steer = turn.steer;
        emit(
            events,
            Event::Finished {
                status: status.into(),
                detail,
            },
        )
        .await?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn register_prompt(
    turn: &mut Turn,
    events: &Sender<Event>,
    wire_id: Value,
    title: &str,
    detail: &str,
    labels: Vec<String>,
    questions: Vec<Question>,
    reply: Reply,
) -> Result<()> {
    let id = wire_id.to_string();
    let prompt = Prompt {
        id: id.clone(),
        title: title.into(),
        detail: clip(detail, MAX_DETAIL),
        choices: labels.clone(),
        questions,
    };
    if turn.pending.contains_key(&id) {
        return Ok(());
    }
    turn.pending.insert(
        id,
        Pending {
            wire_id,
            labels,
            reply,
        },
    );
    emit(events, Event::Prompt(prompt)).await
}
