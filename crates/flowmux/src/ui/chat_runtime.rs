// SPDX-License-Identifier: GPL-3.0-or-later

mod acp;
mod claude;
mod codex;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_channel::{Receiver, Sender};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Provider {
    Codex,
    Claude,
    OpenCode,
    Gemini,
    Cline,
    Antigravity,
}

impl Provider {
    const ALL: [Self; 6] = [
        Self::Codex,
        Self::Claude,
        Self::OpenCode,
        Self::Gemini,
        Self::Cline,
        Self::Antigravity,
    ];

    pub(super) async fn available() -> Vec<Self> {
        let mut probes = tokio::task::JoinSet::new();
        for provider in Self::ALL {
            if let Some(program) = provider.executable() {
                probes.spawn(async move { (provider, provider.supports_chat(program).await) });
            }
        }
        let mut available = Vec::new();
        while let Some(result) = probes.join_next().await {
            if let Ok((provider, true)) = result {
                available.push(provider);
            }
        }
        Self::ALL
            .into_iter()
            .filter(|provider| available.contains(provider))
            .collect()
    }

    async fn supports_chat(self, executable: PathBuf) -> bool {
        let (args, marker): (&[&str], &str) = match self {
            Self::Codex => (&["app-server", "--help"], "app-server"),
            Self::Claude => (&["--help"], "stream-json"),
            Self::OpenCode => (&["acp", "--help"], "opencode acp"),
            Self::Gemini | Self::Cline => (&["--help"], "--acp"),
            Self::Antigravity => (&["help", "acp"], "acp"),
        };
        let mut command = Command::new(executable);
        command
            .args(args)
            .current_dir("/")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        if let Some(path) = chat_path() {
            command.env("PATH", path);
        }
        matches!(tokio::time::timeout(Duration::from_secs(5), command.output()).await,
            Ok(Ok(output)) if output.status.success()
                && (String::from_utf8_lossy(&output.stdout).contains(marker)
                    || String::from_utf8_lossy(&output.stderr).contains(marker)))
    }

    pub(super) fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|agent| agent.name() == name)
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude",
            Self::OpenCode => "OpenCode",
            Self::Gemini => "Gemini",
            Self::Cline => "Cline",
            Self::Antigravity => "Antigravity",
        }
    }

    fn executable(self) -> Option<PathBuf> {
        flowmux_cli::hook_install::resolve_agent_binary_on_path(
            match self {
                Self::Codex => "codex",
                Self::Claude => "claude",
                Self::OpenCode => "opencode",
                Self::Gemini => "gemini",
                Self::Cline => "cline",
                Self::Antigravity => "agy",
            },
            &chat_path()?,
        )
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

fn chat_path() -> Option<std::ffi::OsString> {
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let extra = std::env::var_os("HOME").into_iter().flat_map(|home| {
        let home = PathBuf::from(home);
        [
            home.join(".local/bin"),
            home.join(".cargo/bin"),
            home.join(".opencode/bin"),
        ]
    });
    std::env::join_paths(
        std::env::split_paths(&existing)
            .filter(|path| path.is_absolute())
            .chain(extra)
            .chain([
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/usr/local/bin"),
            ]),
    )
    .ok()
}

#[derive(Clone, Debug)]
pub(super) struct Choice {
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

#[derive(Clone, Debug, Default)]
pub(super) struct Menu {
    pub choices: Vec<Choice>,
    pub current: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Catalog {
    pub models: Menu,
    pub access: Menu,
    pub efforts: Menu,
    pub model_efforts: HashMap<String, Menu>,
    pub plan: bool,
}

impl Catalog {
    pub fn efforts_for(&self, model: Option<&str>) -> &Menu {
        model
            .and_then(|model| self.model_efforts.get(model))
            .unwrap_or(&self.efforts)
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct Settings {
    pub model: Option<String>,
    pub access: Option<String>,
    pub effort: Option<String>,
    pub plan: bool,
    pub confirmed_access: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TextKind {
    Answer,
    Reasoning,
}

#[derive(Clone, Debug, Default)]
pub(super) struct Change {
    pub path: String,
    pub diff: String,
}

#[derive(Clone, Debug)]
pub(super) struct Work {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub running: bool,
    pub failed: bool,
    pub read: bool,
    pub changes: Vec<Change>,
    pub check: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct Question {
    pub id: String,
    pub title: String,
    pub options: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct Prompt {
    pub id: String,
    pub title: String,
    pub detail: String,
    pub choices: Vec<String>,
    pub questions: Vec<Question>,
}

pub(super) enum Event {
    Ready(Catalog),
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
    },
    Finished {
        status: String,
        detail: Option<String>,
    },
    Rejected(String),
    Notice(String),
    StopFailed(String),
    Offline(String),
}

pub(super) enum Input {
    Send {
        text: String,
        settings: Settings,
    },
    Stop,
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
}

impl Wire {
    fn start(provider: Provider, cwd: &Path) -> Result<Self> {
        let program = provider
            .executable()
            .context("The agent executable is no longer available.")?;
        let mut child = Command::new(program)
            .args(provider.args())
            .current_dir(cwd)
            .env("PATH", chat_path().context("Cannot construct search path")?)
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
}

struct Session {
    key: String,
    catalog: Catalog,
    active: Option<Turn>,
    model_option: Option<String>,
    access_option: Option<String>,
    effort_option: Option<String>,
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

pub(super) fn starts_with_elevated_access(choice: &Choice) -> bool {
    choice.elevated()
}

pub(super) fn start(
    runtime: &tokio::runtime::Handle,
    provider: Provider,
    cwd: PathBuf,
) -> (Sender<Input>, Receiver<Event>) {
    let (tx, input) = async_channel::bounded(16);
    let (output, rx) = async_channel::bounded(128);
    runtime.spawn(async move {
        let result = tokio::select! {
            result = run(provider, &cwd, &input, &output) => result,
            () = output.closed() => Ok(()),
        };
        if let Err(error) = result {
            let _ = output
                .send(Event::Offline(clip(&format!("{error:#}"), 2048)))
                .await;
        }
    });
    (tx, rx)
}

async fn run(
    provider: Provider,
    cwd: &Path,
    input: &Receiver<Input>,
    events: &Sender<Event>,
) -> Result<()> {
    if !cwd.is_dir() {
        bail!("The working directory is unavailable.");
    }
    if cwd.to_str().is_none() {
        bail!("Chat requires a UTF-8 working directory.");
    }
    let mut wire = Wire::start(provider, cwd)?;
    let mut session = Session::new();
    match provider {
        Provider::Codex => codex::boot(&mut wire, &mut session, cwd).await?,
        Provider::Claude => claude::boot(&mut wire, &mut session).await?,
        _ => acp::boot(&mut wire, &mut session, cwd).await?,
    }
    emit(events, Event::Ready(session.catalog.clone())).await?;
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
                    Err(_) => return Ok(()),
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
                                session.active = Some(Turn { request, settings, ..Turn::default() });
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
                    Ok(Input::Respond { id, selected, answers }) => {
                        if let Some(turn) = session.active.as_mut().filter(|turn| !turn.stop) {
                            respond(&mut wire, turn, events, id, selected, answers).await?;
                        }
                    }
                }
            }
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
            return emit(events, Event::PromptResolved { id, label }).await;
        }
    };
    wire.write(json!({"jsonrpc":"2.0","id":pending.wire_id,"result":result}))
        .await?;
    let label = label.clone();
    turn.pending.remove(&id);
    emit(events, Event::PromptResolved { id, label }).await
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
    if session.active.take().is_some() {
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
