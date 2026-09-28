// SPDX-License-Identifier: GPL-3.0-or-later
//! Automatic disk refresh owns a separate frontend guard until actual apply ACK.
use super::*;
use crate::editor_watch::{Kind, Notice, Watcher};

const QUIET: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Phase {
    Flushing,
    Working,
    Applying,
    Quarantined,
}
impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Flushing => "flushing",
            Self::Working => "working",
            Self::Applying => "applying",
            Self::Quarantined => "quarantined",
        }
    }
}
pub(super) struct Pending {
    pub(super) id: u64,
    notice: Notice,
    pub(super) phase: Phase,
    started: Instant,
    timed_out: bool,
    error: Option<String>,
}
pub(super) struct State {
    watcher: Option<Watcher>,
    notice: Option<Notice>,
    pub(super) pending: Option<Pending>,
    pub(super) activity: Instant,
    attempted: Instant,
    applied_generation: u64,
    completed: u64,
    deferred: u64,
    last_error: Option<String>,
}
impl State {
    pub(super) fn start(
        root: PathBuf,
        surface: SurfaceId,
        instance: Uuid,
        sender: EventSender,
    ) -> Self {
        let result = Watcher::start(root, move |notice| {
            sender.send(Event::Editor(Signal::Watch(surface, instance, notice)))
        });
        let (watcher, last_error) = match result {
            Ok(watcher) => (Some(watcher), None),
            Err(error) => (None, Some(error.to_string())),
        };
        Self {
            watcher,
            notice: None,
            pending: None,
            activity: Instant::now(),
            attempted: Instant::now(),
            applied_generation: 0,
            completed: 0,
            deferred: 0,
            last_error,
        }
    }
    pub(super) fn status(&self) -> Value {
        let watcher = self.watcher.as_ref().map(Watcher::status);
        let ready = watcher.as_ref().is_some_and(|w| w.ready && !w.stopped);
        let error = self
            .last_error
            .as_ref()
            .or_else(|| watcher.as_ref().and_then(|w| w.last_error.as_ref()));
        json!({"mode":if self.watcher.is_some() {"native"} else {"unavailable"},
            "ready":ready,"pending":self.pending.is_some(),
            "generation":watcher.as_ref().map_or(0,|w|w.generation),
            "applied_generation":self.applied_generation,"completed_count":self.completed,
            "deferred_count":self.deferred,"last_error":error,
            "phase":self.pending.as_ref().map(|p|p.phase.name()),
            "timed_out":self.pending.as_ref().is_some_and(|p|p.timed_out),"watcher":watcher})
    }
    fn acknowledge(&mut self, notice: &Notice) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.watcher
                .as_ref()
                .is_some_and(|w| w.acknowledge(notice.epoch, notice.generation)),
            "automatic refresh watcher acknowledgment no longer matches its owner"
        );
        if self
            .notice
            .as_ref()
            .is_some_and(|n| n.epoch == notice.epoch && n.generation == notice.generation)
        {
            self.notice = None;
        }
        Ok(())
    }
}
impl Editor {
    pub(super) fn refresh_send(&self, id: u64, message: &EditorMessageOut) -> anyhow::Result<()> {
        anyhow::ensure!(
            matches!(
                message,
                EditorMessageOut::ReplaceDocument { .. }
                    | EditorMessageOut::DocumentDiskStatus { .. }
            ),
            "automatic refresh received an unexpected document operation"
        );
        let encoded = flowmux_editor::serialize_host_message(&self.surface_string(), message)?;
        let quoted = serde_json::to_string(&encoded)?;
        self.view.view.evaluate_script(&format!(
            "window.flowmuxWindowsEditor.applyDiskRefreshMessage({id},JSON.parse({quoted}))"
        ))?;
        Ok(())
    }
}
impl App {
    pub(super) fn editor_watch_notice(
        &mut self,
        surface: SurfaceId,
        instance: Uuid,
        notice: Notice,
    ) {
        let Some(editor) = self
            .editors
            .get_mut(&surface)
            .filter(|e| e.instance == instance)
        else {
            return;
        };
        if editor
            .refresh
            .watcher
            .as_ref()
            .is_none_or(|w| w.status().epoch != notice.epoch)
        {
            return;
        }
        if notice.kind == Kind::Failed {
            editor.refresh.last_error = notice.error.clone();
            let _ = editor.refresh.acknowledge(&notice);
        } else {
            editor.refresh.notice = Some(notice);
        }
    }
    pub(super) fn editor_refresh_failed(&mut self, surface: SurfaceId, id: u64, error: &str) {
        let Some(editor) = self.editors.get_mut(&surface) else {
            return;
        };
        let Some(pending) = editor.refresh.pending.as_mut().filter(|p| p.id == id) else {
            return;
        };
        pending.phase = Phase::Quarantined;
        pending.error = Some(error.into());
        editor.refresh.last_error = Some(error.into());
        self.editor_sync_failure(surface, error);
    }
    pub(super) fn editor_refresh_deferred(&mut self, surface: SurfaceId, id: u64) {
        let Some(editor) = self.editors.get_mut(&surface) else {
            return;
        };
        if !editor
            .refresh
            .pending
            .as_ref()
            .is_some_and(|p| p.id == id && p.phase == Phase::Flushing)
        {
            return;
        }
        editor.refresh.pending = None;
        editor.refresh.deferred = editor.refresh.deferred.saturating_add(1);
        editor.refresh.attempted = Instant::now();
        editor.refresh_ready();
    }
    pub(super) fn editor_refresh_flush(
        &mut self,
        surface: SurfaceId,
        id: u64,
        error: Option<String>,
    ) -> bool {
        if self.editors[&surface]
            .refresh
            .pending
            .as_ref()
            .is_none_or(|p| p.id != id)
        {
            return false;
        }
        if !self.editors[&surface]
            .refresh
            .pending
            .as_ref()
            .is_some_and(|p| p.phase == Phase::Flushing)
        {
            return true;
        }
        if let Some(error) = error {
            self.editor_refresh_failed(surface, id, &error);
            return true;
        }
        match self.editor_work(surface, id, Work::PollDisk) {
            Ok(()) => {
                self.editors
                    .get_mut(&surface)
                    .unwrap()
                    .refresh
                    .pending
                    .as_mut()
                    .unwrap()
                    .phase = Phase::Working
            }
            Err(error) => {
                // Submission failed: no disk operation owns the guard. The
                // matching frontend state can safely release it before retry.
                let editor = self.editors.get_mut(&surface).unwrap();
                let abort = editor.view.view.evaluate_script(&format!(
                    "window.flowmuxWindowsEditor.abortDiskRefreshBeforeWork({id})"
                ));
                if let Err(abort) = abort {
                    self.editor_refresh_failed(surface, id, &abort.to_string());
                } else {
                    editor.refresh.last_error = Some(error.to_string());
                    self.editor_refresh_deferred(surface, id);
                }
            }
        }
        true
    }
    pub(super) fn editor_refresh_worker_done(
        &mut self,
        surface: SurfaceId,
        id: u64,
        error: Option<String>,
    ) -> anyhow::Result<()> {
        let editor = self.editors.get_mut(&surface).unwrap();
        let pending = editor
            .refresh
            .pending
            .as_mut()
            .context("automatic refresh lost its worker owner")?;
        anyhow::ensure!(
            pending.id == id && pending.phase == Phase::Working,
            "automatic refresh completed in the wrong phase"
        );
        pending.phase = Phase::Applying;
        pending.error = error;
        editor.view.view.evaluate_script(&format!(
            "window.flowmuxWindowsEditor.completeDiskRefresh({id})"
        ))?;
        Ok(())
    }
    pub(super) fn editor_refresh_applied(
        &mut self,
        surface: SurfaceId,
        id: u64,
    ) -> anyhow::Result<()> {
        let editor = self.editors.get_mut(&surface).unwrap();
        if !editor
            .refresh
            .pending
            .as_ref()
            .is_some_and(|p| p.id == id && p.phase == Phase::Applying)
        {
            return Ok(());
        }
        // Retain the pending owner until both acknowledgments are accepted.
        // A failure must quarantine this guard, never silently lose its owner.
        let notice = editor.refresh.pending.as_ref().unwrap().notice.clone();
        editor.refresh.acknowledge(&notice)?;
        editor.view.view.evaluate_script(&format!(
            "window.flowmuxWindowsEditor.releaseDiskRefresh({id})"
        ))?;
        let mut pending = editor.refresh.pending.take().unwrap();
        pending.timed_out |= pending.started.elapsed() >= crate::editor_open::OPEN_BUDGET;
        editor.refresh.applied_generation = pending.notice.generation;
        editor.refresh.completed = editor.refresh.completed.saturating_add(1);
        let unknown = editor
            .documents
            .as_array()
            .is_some_and(|docs| docs.iter().any(|d| d["disk_status_known"] == false));
        editor.refresh.last_error = pending.error.or_else(|| {
            if unknown { Some("disk status remains unknown after an incomplete scan; inspect the affected documents".into()) }
            else if pending.timed_out { Some("automatic refresh exceeded its deadline and has now finished".into()) }
            else { None }
        });
        editor.refresh.activity = Instant::now();
        editor.refresh_ready();
        Ok(())
    }
    pub(in crate::native::host) fn editor_refresh_tick(&mut self) {
        for editor in self.editors.values_mut() {
            if let Some(pending) = &mut editor.refresh.pending {
                if !pending.timed_out
                    && pending.phase != Phase::Quarantined
                    && pending.started.elapsed() >= crate::editor_open::OPEN_BUDGET
                {
                    pending.timed_out = true;
                    editor.refresh.last_error = Some("automatic refresh timed out; input remains guarded until the original result is applied".into());
                }
            }
        }
        if self.closing
            || self.close_accepted
            || self.close_request.is_some()
            || self.editor_barrier.is_some()
            || self.pending_save.is_some()
            || self.editor_picker_pending
        {
            return;
        }
        // At most one new operation per timer tick. Least recently attempted
        // comes first, so a noisy root cannot permanently starve another editor.
        let candidate = self
            .editors
            .iter()
            .filter(|(_, e)| {
                e.ready
                    && e.pending.is_none()
                    && e.refresh.pending.is_none()
                    && e.inflight.is_empty()
                    && e.replacements.is_empty()
                    && e.documents.as_array().is_some_and(|docs| !docs.is_empty())
                    && e.refresh.notice.is_some()
                    && e.refresh.activity.elapsed() >= QUIET
                    && e.refresh.attempted.elapsed() >= QUIET
                    && e.refresh.watcher.as_ref().is_some_and(|w| {
                        let s = w.status();
                        s.ready && !s.stopped
                    })
            })
            .min_by_key(|(_, e)| e.refresh.attempted)
            .map(|(id, _)| *id);
        let Some(surface) = candidate else {
            return;
        };
        let id = self.editor_next();
        let editor = self.editors.get_mut(&surface).unwrap();
        let notice = editor.refresh.notice.clone().unwrap();
        editor.refresh.pending = Some(Pending {
            id,
            notice,
            phase: Phase::Flushing,
            started: Instant::now(),
            timed_out: false,
            error: None,
        });
        editor.refresh.attempted = Instant::now();
        editor.refresh_ready();
        if let Err(error) = editor.view.view.evaluate_script(&format!(
            "window.flowmuxWindowsEditor.beginDiskRefresh({id})"
        )) {
            self.editor_refresh_failed(surface, id, &error.to_string());
        }
    }
}
