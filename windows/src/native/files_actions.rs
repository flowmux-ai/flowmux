// SPDX-License-Identifier: GPL-3.0-or-later
//! One host-wide prepared file operation, with retained outcomes.
use super::*;
use crate::{files_operation_fs as fs, files_operations as operations};
use std::collections::VecDeque;

#[derive(Default)]
pub(super) struct State {
    service: Option<operations::Service>,
    active: Option<Active>,
    receipts: VecDeque<Value>,
}
struct Active {
    ticket: operations::Ticket,
    owner: domain::Owner,
    root: PathBuf,
    source: String,
    kind: fs::Kind,
    destination: String,
    reply: Option<ipc::Reply>,
    committing: bool,
    accepted: bool,
    cancel_requested: bool,
}
impl App {
    pub(super) fn files_operation_next_tick(&self) -> Option<Instant> {
        self.files
            .actions
            .active
            .as_ref()
            .map(|_| Instant::now() + Duration::from_millis(100))
    }
    pub(super) fn files_operation_current(&self) -> Value {
        self.files
            .actions
            .active
            .as_ref()
            .and_then(|a| self.files_operation_status(a.ticket.id).ok())
            .map_or(Value::Null, |v| v["operation"].clone())
    }
    pub(in crate::native::host) fn files_operation_guard(&self) -> anyhow::Result<()> {
        if let Some(active) = &self.files.actions.active {
            anyhow::bail!("Files operation {} is active; query files operation-status or cancel it before retrying", active.ticket.id);
        }
        Ok(())
    }
    fn files_operation_status(&self, id: Uuid) -> anyhow::Result<Value> {
        if let Some(active) = self
            .files
            .actions
            .active
            .as_ref()
            .filter(|a| a.ticket.id == id)
        {
            return Ok(
                json!({"operation":{"id":id,"status":if active.committing {"accepted"} else {"preparing"},"kind":active.kind,"source":active.source,"destination":active.destination,"accepted":active.accepted,"cancel_requested":active.cancel_requested,"owner":active.owner}}),
            );
        }
        self.files.actions.receipts.iter().find(|v| v["operation"]["id"] == id.to_string()).cloned().context("Files operation receipt not found; only the latest 64 terminal receipts are retained")
    }
    pub(super) fn files_operation_command(
        &mut self,
        op: domain::Op,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<Option<Value>> {
        match op {
            domain::Op::OperationStatus(args) => Ok(Some(self.files_operation_status(args.id)?)),
            domain::Op::OperationCancel(args) => {
                if let Some(active) = self
                    .files
                    .actions
                    .active
                    .as_mut()
                    .filter(|a| a.ticket.id == args.id)
                {
                    active.ticket.cancel();
                    active.cancel_requested = true;
                }
                Ok(Some(self.files_operation_status(args.id)?))
            }
            domain::Op::Copy(args) => self.files_operation_start(
                args.pane,
                args.token,
                args.index,
                args.destination,
                fs::Kind::Copy,
                reply,
            ),
            domain::Op::Move(args) => self.files_operation_start(
                args.pane,
                args.token,
                args.index,
                args.destination,
                fs::Kind::Move,
                reply,
            ),
            domain::Op::Rename(args) => self.files_operation_start(
                args.pane,
                args.token,
                args.index,
                args.name,
                fs::Kind::Rename,
                reply,
            ),
            _ => anyhow::bail!("not a Files operation"),
        }
    }
    fn files_operation_start(
        &mut self,
        pane: Uuid,
        token: Uuid,
        index: usize,
        destination: String,
        kind: fs::Kind,
        reply: Option<ipc::Reply>,
    ) -> anyhow::Result<Option<Value>> {
        self.files_operation_guard()?;
        self.files_reconcile();
        let state = self
            .files
            .states
            .get(&PaneId(pane))
            .context("Files pane not found")?;
        anyhow::ensure!(
            self.files_owner_current(&state.owner) && state.pending.is_none(),
            "Files is hidden, stale or loading"
        );
        if kind == fs::Kind::Rename {
            anyhow::ensure!(
                !destination.contains(['/', '\\']),
                "Rename needs one new file name, not a path"
            );
        }
        let path = state.model.open_path(token, index)?;
        let source = path
            .to_str()
            .context("Files source is not Unicode")?
            .replace('\\', "/");
        let root = state.root.clone();
        let owner = state.owner.clone();
        self.editor_files_operation_guard(&root, &source)?;
        if self.files.actions.service.is_none() {
            let sender = self.sender.clone();
            self.files.actions.service = Some(operations::Service::start(move |event| {
                sender.send(Event::Files(Signal::Operation(event)))
            })?);
        }
        let submitted = reply
            .as_ref()
            .map_or_else(Instant::now, ipc::Reply::received_at);
        let request = fs::Request {
            kind,
            root: root.clone(),
            source: source.clone(),
            destination: destination.clone(),
        };
        let ticket = self
            .files
            .actions
            .service
            .as_ref()
            .unwrap()
            .prepare(request, submitted)?;
        self.files.actions.active = Some(Active {
            ticket,
            owner,
            root,
            source,
            kind,
            destination,
            reply,
            committing: false,
            accepted: false,
            cancel_requested: false,
        });
        ACTIVE_OPERATION.with(|id| id.set(self.files.actions.active.as_ref().map(|a| a.ticket.id)));
        self.files_schedule();
        Ok(None)
    }
    fn files_operation_finish(&mut self, state: &str, result: Value) {
        let Some(active) = self.files.actions.active.take() else {
            return;
        };
        ACTIVE_OPERATION.with(|id| id.set(None));
        if let Some(reply) = &active.reply {
            if !active.accepted {
                let _=reply.try_send(json!({"error":result["error"].as_str().unwrap_or("Files operation was not admitted"),"id":active.ticket.id}));
            }
        }
        let receipt = json!({"operation":{"id":active.ticket.id,"status":state,"kind":active.kind,"destination":active.destination,"accepted":active.accepted,"cancel_requested":active.cancel_requested,"owner":active.owner,"source":active.source,"error":result.get("error"),"outcome":result}});
        self.files.actions.receipts.push_back(receipt);
        while self.files.actions.receipts.len() > 64 {
            self.files.actions.receipts.pop_front();
        }
        if self.files_owner_current(&active.owner) {
            let pane = PaneId(active.owner.pane);
            if state == "succeeded" && active.kind != fs::Kind::Copy {
                if let Some(destination) = result["destination"].as_str() {
                    let mut candidate = self.files.states[&pane].model.clone();
                    candidate.remap_selected_path(&active.source, destination);
                    if let Err(error) = self.files_commit_model(pane, candidate) {
                        if let Some(receipt) = self.files.actions.receipts.back_mut() {
                            receipt["operation"]["warning"] = json!(format!(
                                "File operation succeeded, but selection could not be remapped: {}",
                                domain::short_error(error)
                            ));
                        }
                    }
                }
            }
            if let Some(current) = self.files.states.get_mut(&pane) {
                current
                    .model
                    .fail("File operation completed; refreshing the listing");
                current.message = Some(format!("Operation {}: {}", active.ticket.id, state));
                let _ = current.render();
            }
            if let Err(error) = self.files_scan(pane, None, None) {
                if let Some(current) = self.files.states.get_mut(&pane) {
                    current.message = Some(format!(
                        "Operation {}: {}; refresh failed: {}",
                        active.ticket.id,
                        state,
                        domain::short_error(error)
                    ));
                    let _ = current.render();
                }
            }
        }
        self.browser_resume_closed();
    }
    pub(super) fn files_operation_event(&mut self, event: operations::Event) -> anyhow::Result<()> {
        match event {
            operations::Event::Prepared(ready) => {
                let Some(active) = self
                    .files
                    .actions
                    .active
                    .as_ref()
                    .filter(|a| a.ticket.id == ready.id)
                else {
                    return Ok(());
                };
                let failure = ready.error.clone().or_else(|| {
                    if !self.files_owner_current(&active.owner)
                        || active.ticket.is_cancelled()
                        || active.ticket.is_expired()
                    {
                        Some(
                            "Files preparation expired or its owner changed; no commit submitted"
                                .into(),
                        )
                    } else {
                        self.editor_files_operation_guard(&active.root, &active.source)
                            .err()
                            .map(|e| e.to_string())
                    }
                });
                if let Some(error) = failure {
                    drop(ready);
                    self.files_operation_finish("failed", json!({"error":error}));
                    return Ok(());
                }
                let active = self.files.actions.active.as_mut().unwrap();
                let receipt = json!({"operation":{"accepted":true,"id":active.ticket.id,"status":"accepted","kind":active.kind,"source":active.source,"destination":active.destination}});
                if active
                    .reply
                    .as_ref()
                    .is_some_and(|reply| reply.try_send(receipt).is_err())
                {
                    active.ticket.cancel();
                    drop(ready);
                    self.files_operation_finish("cancelled",json!({"error":"Operation receipt could not be delivered; no commit submitted"}));
                    return Ok(());
                }
                active.accepted = true;
                active.committing = true;
                if let Err(error) = self.files.actions.service.as_ref().unwrap().commit(ready) {
                    self.files_operation_finish("failed", json!({"error":error.to_string()}));
                }
            }
            operations::Event::Finished(finished) => {
                if self
                    .files
                    .actions
                    .active
                    .as_ref()
                    .is_none_or(|a| a.ticket.id != finished.id)
                {
                    return Ok(());
                }
                match finished.result {
                    Ok(outcome) => {
                        self.files_operation_finish("succeeded", serde_json::to_value(outcome)?)
                    }
                    Err(error) => self.files_operation_finish("failed", json!({"error":error})),
                }
            }
        }
        Ok(())
    }
    pub(super) fn files_operation_tick(&mut self) {
        let stale = self
            .files
            .actions
            .active
            .as_ref()
            .is_some_and(|a| !self.files_owner_current(&a.owner));
        if let Some(active) = &mut self.files.actions.active {
            if stale
                || active.ticket.is_cancelled()
                || (!active.committing && active.ticket.is_expired())
            {
                active.ticket.cancel();
                active.cancel_requested = true;
                if !active.accepted {
                    if let Some(reply) = active.reply.take() {
                        let _=reply.try_send(json!({"error":"Files operation preparation expired or was cancelled; no commit submitted","id":active.ticket.id}));
                    }
                }
            }
        }
    }
}
