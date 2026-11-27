// SPDX-License-Identifier: GPL-3.0-or-later

use crate::chat_runtime::{self, Catalog, Event, Input, Provider, Settings};
use flowmux_core::SurfaceId;
use flowmux_state::chat_history::{self, Record, Store};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::oneshot;

#[derive(Serialize, Deserialize)]
pub struct Launch {
    pub provider: Provider,
    pub program: PathBuf,
    pub cwd: PathBuf,
    pub environment: Vec<(String, String)>,
    pub native_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub enum Request {
    Attach {
        id: SurfaceId,
        launch: Option<Launch>,
    },
    Read {
        id: SurfaceId,
        after: u64,
    },
    Input {
        id: SurfaceId,
        input: Input,
    },
}

#[derive(Serialize, Deserialize)]
pub enum Response {
    Missing,
    Attached {
        sequence: u64,
        through: i64,
        busy: bool,
        catalog: Option<Box<Catalog>>,
        settings: Settings,
    },
    Events {
        sequence: u64,
        events: Vec<Event>,
        ended: bool,
    },
    Accepted,
}

#[derive(Default)]
pub(super) struct Chats(Mutex<HashMap<SurfaceId, Chat>>);

struct Chat {
    owner: Option<u64>,
    input: async_channel::Sender<(Input, oneshot::Sender<Result<(), String>>)>,
    snapshot: Arc<Mutex<Snapshot>>,
}

#[derive(Default)]
struct Snapshot {
    sequence: u64,
    through: i64,
    catalog: Option<Catalog>,
    settings: Settings,
    started: Option<Instant>,
    events: VecDeque<Event>,
    ended: bool,
}

impl Chats {
    pub(super) fn has_live_chats(&self) -> bool {
        self.0
            .lock()
            .unwrap()
            .values()
            .any(|chat| !chat.input.is_closed())
    }

    pub(super) fn disconnect(&self, connection: u64) {
        let mut chats = self.0.lock().unwrap();
        for chat in chats
            .values_mut()
            .filter(|chat| chat.owner == Some(connection))
        {
            chat.owner = None;
        }
        chats.retain(|_, chat| chat.owner.is_some() || !chat.input.is_closed());
    }

    pub(super) fn stop(&self) {
        for chat in self.0.lock().unwrap().values() {
            chat.input.close();
        }
    }

    pub(super) async fn request(
        &self,
        connection: u64,
        request: Request,
    ) -> anyhow::Result<Response> {
        match request {
            Request::Attach { id, launch } => {
                let mut chats = self.0.lock().unwrap();
                if chats.get(&id).is_some_and(|chat| chat.input.is_closed()) {
                    chats.remove(&id);
                }
                anyhow::ensure!(
                    chats.contains_key(&id) || launch.is_none() || chats.len() < 32,
                    "Agent Host chat limit reached"
                );
                if let std::collections::hash_map::Entry::Vacant(slot) = chats.entry(id) {
                    let Some(launch) = launch else {
                        return Ok(Response::Missing);
                    };
                    anyhow::ensure!(
                        launch.program.is_absolute() && launch.cwd.is_absolute(),
                        "Invalid chat launch paths"
                    );
                    let (input, receiver) = async_channel::bounded(16);
                    let lock = chat_history::lock(id)?;
                    let store = Store::open()?;
                    // Restore the last selection; an elevated access mode asks again.
                    let settings = Settings {
                        confirmed_access: false,
                        ..store.settings(id).ok().flatten().unwrap_or_default()
                    };
                    let snapshot = Arc::new(Mutex::new(Snapshot {
                        through: store.last_record(id)?,
                        settings,
                        ..Snapshot::default()
                    }));
                    tokio::spawn(run(id, launch, receiver, snapshot.clone(), store, lock));
                    slot.insert(Chat {
                        owner: None,
                        input,
                        snapshot,
                    });
                }
                let chat = chats.get_mut(&id).unwrap();
                anyhow::ensure!(
                    chat.owner.is_none_or(|owner| owner == connection),
                    "This chat is already open in another window"
                );
                chat.owner = Some(connection);
                let snapshot = chat.snapshot.lock().unwrap();
                Ok(Response::Attached {
                    sequence: snapshot.sequence,
                    through: snapshot.through,
                    busy: snapshot.started.is_some(),
                    catalog: snapshot.catalog.clone().map(Box::new),
                    settings: snapshot.settings.clone(),
                })
            }
            Request::Read { id, after } => {
                let chats = self.0.lock().unwrap();
                let Some(chat) = chats.get(&id) else {
                    return Ok(Response::Missing);
                };
                anyhow::ensure!(chat.owner == Some(connection), "Chat attachment has ended");
                let snapshot = chat.snapshot.lock().unwrap();
                let first = snapshot.sequence - snapshot.events.len() as u64;
                anyhow::ensure!(
                    (first..=snapshot.sequence).contains(&after),
                    "Chat has newer saved output; reconnect to catch up"
                );
                let events: Vec<_> = snapshot
                    .events
                    .iter()
                    .skip((after - first) as usize)
                    .take(8)
                    .cloned()
                    .collect();
                let sequence = after + events.len() as u64;
                Ok(Response::Events {
                    sequence,
                    events,
                    ended: snapshot.ended && sequence == snapshot.sequence,
                })
            }
            Request::Input { id, input } => {
                let sender = {
                    let chats = self.0.lock().unwrap();
                    let chat = chats
                        .get(&id)
                        .ok_or_else(|| anyhow::anyhow!("Chat session has ended"))?;
                    anyhow::ensure!(chat.owner == Some(connection), "Chat attachment has ended");
                    chat.input.clone()
                };
                let (reply, result) = oneshot::channel();
                sender
                    .try_send((input, reply))
                    .map_err(|_| anyhow::anyhow!("Chat is unavailable or busy"))?;
                result.await?.map_err(anyhow::Error::msg)?;
                Ok(Response::Accepted)
            }
        }
    }
}

async fn run(
    id: SurfaceId,
    launch: Launch,
    input: async_channel::Receiver<(Input, oneshot::Sender<Result<(), String>>)>,
    shared: Arc<Mutex<Snapshot>>,
    mut store: Store,
    _lock: flowmux_state::InstanceLock,
) {
    let Launch {
        provider,
        program,
        cwd,
        environment,
        native_id,
    } = launch;
    let (commands, events, provider_task) = chat_runtime::start(
        &tokio::runtime::Handle::current(),
        provider,
        program,
        cwd.clone(),
        environment,
        native_id,
    );
    let save = |store: &mut Store, state: &mut Snapshot, record| -> Result<(), String> {
        store
            .append(id, provider.name(), &cwd, &[record])
            .map_err(|err| err.to_string())?;
        state.through = store.last_record(id).map_err(|err| err.to_string())?;
        Ok(())
    };
    loop {
        tokio::select! {
            next = input.recv() => {
                let Ok((next, reply)) = next else { break };
                if matches!(next, Input::End) {
                    let _ = reply.send(Ok(()));
                    break;
                }
                let accepted = (|| {
                    let mut state = shared.lock().unwrap();
                    if commands.is_closed() || commands.is_full() {
                        return Err("Agent is unavailable or busy".into());
                    }
                    if let Input::Send { text, settings } = &next {
                        if state.started.is_some() || state.catalog.is_none() {
                            return Err("Chat is not ready to start another turn".into());
                        }
                        save(&mut store, &mut state, Record::Begin { prompt: text.clone() })?;
                        state.started = Some(Instant::now());
                        state.settings = settings.clone();
                        // A plan turn's read-only access is not the chat's selection.
                        if !settings.plan {
                            let saved = Settings { confirmed_access: false, ..settings.clone() };
                            if let Err(error) = store.save_settings(id, &saved) {
                                tracing::warn!(%error, "unable to save chat settings");
                            }
                        }
                    }
                    commands.try_send(next).map_err(|_| "Agent is unavailable or busy".into())
                })();
                let _ = reply.send(accepted);
            }
            event = events.recv() => {
                let Ok(event) = event else { break };
                let is_offline = matches!(event, Event::Offline(_));
                let mut state = shared.lock().unwrap();
                let finish = |state: &mut Snapshot, status: &str, detail| {
                    state.started.take().map(|start| Record::Finished {
                        status: status.to_owned(), detail, elapsed: start.elapsed().as_secs(),
                    })
                };
                let record = match &event {
                    Event::Ready(catalog) => {state.catalog = Some(catalog.clone()); None}
                    Event::Session(native_id) => Some(Record::Session { native_id: native_id.clone() }),
                    Event::Text { id, kind, text, append } => Some(Record::Text {
                        id: id.clone(), kind: *kind, text: text.clone(), append: *append,
                    }),
                    Event::Work(work) => Some(Record::Work(work.clone())),
                    Event::Prompt(prompt) => Some(Record::Prompt(prompt.clone())),
                    Event::PromptResolved { id, label, answers } => Some(Record::Resolved {
                        id: id.clone(), label: label.clone(), answers: answers.clone(),
                    }),
                    Event::Finished { status, detail } => finish(&mut state, status, detail.clone()),
                    Event::Rejected(error) => finish(&mut state, "Not sent", Some(error.clone())),
                    Event::Offline(error) => finish(&mut state, "Disconnected", Some(error.clone())),
                    _ => None,
                };
                if let Some(record) = record {
                    if let Err(error) = save(&mut store, &mut state, record) {
                        state.push(Event::Offline(format!("Could not save chat history: {error}")));
                        break;
                    }
                }
                state.push(event);
                if is_offline { break; }
            }
        }
    }
    drop(commands);
    drop(events);
    let _ = provider_task.await;
    let mut state = shared.lock().unwrap();
    if let Some(started) = state.started.take() {
        let status = "Interrupted".to_owned();
        let record = Record::Finished {
            status: status.clone(),
            detail: None,
            elapsed: started.elapsed().as_secs(),
        };
        if let Err(error) = save(&mut store, &mut state, record) {
            state.push(Event::Offline(format!(
                "Could not save chat history: {error}"
            )));
        } else {
            state.push(Event::Finished {
                status,
                detail: None,
            });
        }
    }
    state.ended = true;
}

impl Snapshot {
    fn push(&mut self, event: Event) {
        if self.events.len() == 256 {
            self.events.pop_front();
        }
        self.events.push_back(event);
        self.sequence += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chat_attachment_does_not_own_the_execution() {
        let chats = Chats::default();
        let id = SurfaceId::new();
        let (input, receiver) = async_channel::bounded(1);
        let mut snapshot = Snapshot {
            started: Some(Instant::now()),
            ..Snapshot::default()
        };
        snapshot.push(Event::Notice("Retained".into()));
        chats.0.lock().unwrap().insert(
            id,
            Chat {
                owner: None,
                input,
                snapshot: Arc::new(Mutex::new(snapshot)),
            },
        );
        let attach = || Request::Attach { id, launch: None };
        assert!(matches!(
            chats.request(1, attach()).await.unwrap(),
            Response::Attached {
                sequence: 1,
                busy: true,
                ..
            }
        ));
        assert!(chats.request(2, attach()).await.is_err());
        chats.disconnect(1);
        assert!(chats.has_live_chats());
        assert!(!receiver.is_closed());
        assert!(matches!(
            chats.request(2, attach()).await.unwrap(),
            Response::Attached {
                sequence: 1,
                busy: true,
                ..
            }
        ));
        chats.stop();
        assert!(receiver.is_closed());
    }
}
