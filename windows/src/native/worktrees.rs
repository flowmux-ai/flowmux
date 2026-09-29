// SPDX-License-Identifier: GPL-3.0-or-later
//! One cancellable Git operation for the focused local workspace's right dock.
use super::*;
use crate::worktrees::{Info, List, RemoveError};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) enum Signal {
    Ui(Uuid, worktree_panel::UiAction),
    Loaded(Uuid, Result<List, String>),
    Removed(Uuid, PathBuf, Result<(), RemoveError>),
    Choice(Uuid, bool),
}
#[derive(Clone, PartialEq, Eq, serde::Serialize)]
struct Source {
    surface: SurfaceId,
    workspace: WorkspaceId,
    cwd: PathBuf,
    ssh: bool,
}
struct Job {
    id: Uuid,
    source: Source,
    cancel: Arc<AtomicBool>,
    removing: bool,
}
struct Decision {
    id: Uuid,
    kind: &'static str,
    path: PathBuf,
    panel: editor::ClosePanel,
}
#[derive(Default)]
pub(super) struct Controller {
    open: bool,
    id: Uuid,
    source: Option<Source>,
    list: Option<List>,
    error: String,
    job: Option<Job>,
    refresh: bool,
    panel: Option<worktree_panel::Panel>,
    decision: Option<Decision>,
}
impl Controller {
    #[cfg(debug_assertions)]
    pub(super) fn capture_window(&self) -> Option<HWND> {
        self.panel
            .as_ref()
            .filter(|_| self.open && self.decision.is_none())
            .and_then(worktree_panel::Panel::capture_window)
    }
    pub(super) fn status(&self) -> Value {
        json!({"open":self.open,"id":self.id,"source":self.source,"list":self.list,
            "error":self.error,"loading":self.job.is_some() || self.refresh,
            "panel":self.panel.as_ref().map(worktree_panel::Panel::status),
            "dialog":self.decision.as_ref().map(|d|d.panel.diagnostics()),
            "decision":self.decision.as_ref().map(|d|d.kind)})
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        self.decision
            .as_ref()
            .is_some_and(|d| d.panel.handle_message(message))
            || self
                .panel
                .as_ref()
                .is_some_and(|p| p.handle_message(message))
    }
    pub(super) fn shutdown(&mut self) {
        if let Some(job) = &self.job {
            job.cancel.store(true, Ordering::Release);
        }
        self.open = false;
        self.refresh = false;
        self.decision.take();
        self.panel.take();
    }
    pub(super) fn refresh_theme(&mut self) -> anyhow::Result<()> {
        if let Some(panel) = &mut self.panel {
            panel.refresh_theme()?;
        }
        Ok(())
    }
}

// Component boundaries matter (a sibling named repo-old is not inside repo).
// Canonical paths from Git can carry Windows' extended-length prefix.
fn within(path: &Path, root: &Path) -> bool {
    let normalize = |p: &Path| {
        let path = p.to_string_lossy().replace('/', "\\");
        let path = if let Some(unc) = path.strip_prefix("\\\\?\\UNC\\") {
            format!("\\\\{unc}")
        } else {
            path.trim_start_matches("\\\\?\\").to_owned()
        };
        path.trim_end_matches('\\').to_lowercase()
    };
    let path = normalize(path);
    let root = normalize(root);
    path == root
        || path
            .strip_prefix(&root)
            .is_some_and(|tail| tail.starts_with('\\'))
}
fn block_reason(info: &Info, used: &[PathBuf]) -> Option<String> {
    if info.is_main {
        Some("Main worktree cannot be removed".into())
    } else if info.is_current {
        Some("Current worktree cannot be removed".into())
    } else if info.is_bare {
        Some("Bare repository cannot be removed as a worktree".into())
    } else if let Some(reason) = &info.lock_reason {
        Some(format!(
            "Locked worktree: {}",
            if reason.is_empty() {
                "no reason provided"
            } else {
                reason
            }
        ))
    } else if used.iter().any(|cwd| within(cwd, &info.path)) {
        Some("Close tabs and workspaces using this worktree before removal".into())
    } else {
        None
    }
}

impl App {
    fn worktree_source(&self) -> Option<Source> {
        let workspace = self.current_workspace()?;
        let surface = workspace.active();
        let (_, _, cwd) = self.locate(surface)?;
        Some(Source {
            surface,
            workspace: workspace.id,
            cwd,
            ssh: workspace.ssh.is_some(),
        })
    }
    fn worktree_used_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for workspace in self.workspaces.iter().filter(|w| w.ssh.is_none()) {
            paths.push(workspace.cwd.clone());
            for (_, _, tabs) in workspace.leaves() {
                for tab in tabs {
                    match &tab.kind {
                        SurfaceKind::Terminal { cwd: Some(cwd), .. } => paths.push(cwd.clone()),
                        SurfaceKind::Editor { workspace_root, .. } => {
                            paths.push(workspace_root.clone())
                        }
                        _ => {}
                    }
                }
            }
        }
        paths
    }
    pub(super) fn toggle_worktrees(&mut self) -> anyhow::Result<()> {
        self.worktree_guard()?;
        if self.worktrees.open {
            self.worktrees.shutdown();
        } else {
            self.worktrees.id = Uuid::new_v4();
            self.worktrees.panel = Some(worktree_panel::Panel::new(
                self.window,
                self.background_test,
                self.worktrees.id,
            )?);
            self.worktrees.open = true;
            self.worktrees.source = None;
            self.worktrees.refresh = true;
            self.worktrees_reconcile()?;
        }
        self.layout()
    }
    fn worktree_focus_out(&mut self, direction: FocusDirection) -> anyhow::Result<()> {
        let source = self
            .worktrees
            .source
            .as_ref()
            .map(|source| source.surface)
            .or_else(|| self.current_surface())
            .context("Worktree source is unavailable")?;
        if matches!(direction, FocusDirection::Right) && self.files_focus_visible() {
            return Ok(());
        }
        if !matches!(direction, FocusDirection::Left)
            && self.focus_direction(source, direction)?.is_some()
        {
            return Ok(());
        }
        self.select(source)?;
        self.focus_active()
    }
    fn worktree_guard(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.closing
                && !self.close_accepted
                && self.close_request.is_none()
                && self.editor_barrier.is_none()
                && self.pending_save.is_none()
                && !self.overview.is_open()
                && self.worktrees.decision.is_none()
                && unsafe { IsWindowEnabled(self.window) } != 0,
            "Window is busy"
        );
        Ok(())
    }
    pub(super) fn worktrees_reconcile(&mut self) -> anyhow::Result<()> {
        if !self.worktrees.open {
            return Ok(());
        }
        let source = self.worktree_source();
        if self.worktrees.source != source {
            if let Some(job) = &self.worktrees.job {
                job.cancel.store(true, Ordering::Release);
            }
            self.worktrees.source = source;
            self.worktrees.list = None;
            self.worktrees.decision.take();
            self.worktrees.refresh = true;
            self.worktrees.error = String::new();
        }
        if self.worktrees.refresh && self.worktrees.job.is_none() {
            self.worktrees.refresh = false;
            match self.worktrees.source.clone() {
                Some(source) if !source.ssh => {
                    let id = Uuid::new_v4();
                    let cancel = Arc::new(AtomicBool::new(false));
                    let token = cancel.clone();
                    let cwd = source.cwd.clone();
                    let sender = self.sender.clone();
                    std::thread::Builder::new()
                        .name("worktrees-list".into())
                        .spawn(move || {
                            let result = std::panic::catch_unwind(|| {
                                super::super::worktree_git::list(&cwd, &token)
                            })
                            .unwrap_or_else(|_| Err("Worktree worker failed".into()));
                            sender.send(Event::Worktrees(Signal::Loaded(id, result)));
                        })?;
                    self.worktrees.job = Some(Job {
                        id,
                        source,
                        cancel,
                        removing: false,
                    });
                }
                Some(_) => {
                    self.worktrees.error = "Worktrees are unavailable for SSH workspaces".into()
                }
                None => self.worktrees.error = "No active workspace for Worktrees".into(),
            }
        }
        self.worktrees_render()
    }
    fn worktrees_render(&mut self) -> anyhow::Result<()> {
        let used = self.worktree_used_paths();
        let busy = self.worktrees.job.is_some();
        let rows = self
            .worktrees
            .list
            .as_ref()
            .map(|list| {
                list.items
                    .iter()
                    .map(|info| worktree_panel::Row {
                        info: info.clone(),
                        remove_block_reason: block_reason(info, &used),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let title = self
            .worktrees
            .list
            .as_ref()
            .map(|list| {
                list.repository_root
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|| "Worktrees".into());
        let status = if busy && self.worktrees.error.is_empty() {
            if self.worktrees.job.as_ref().is_some_and(|job| job.removing) {
                "Removing worktree…"
            } else {
                "Loading worktrees…"
            }
        } else {
            &self.worktrees.error
        };
        if let Some(panel) = &mut self.worktrees.panel {
            panel.set_state(&title, status, rows, busy)?;
        }
        Ok(())
    }
    pub(super) fn worktrees_width(&self, width: i32, scale: f64) -> i32 {
        if self.worktrees.open {
            let available = (width - (160.0 * scale).round() as i32).max(0);
            if available < (200.0 * scale).round() as i32 {
                0
            } else {
                ((300.0 * scale).round() as i32).min(available)
            }
        } else {
            0
        }
    }
    pub(super) fn worktrees_layout(&mut self, area: Option<model::Rect>, scale: f64) {
        if let Some(panel) = &mut self.worktrees.panel {
            panel.layout(area, scale, self.background_test);
        }
    }
    fn worktree_dialog(&mut self, kind: &'static str, path: PathBuf) -> anyhow::Result<()> {
        self.worktree_guard()?;
        let list = self
            .worktrees
            .list
            .as_ref()
            .context("Worktree list is unavailable")?;
        let info = list
            .items
            .iter()
            .find(|row| row.path == path)
            .context("Worktree is no longer in the list")?;
        if kind != "info" {
            anyhow::ensure!(
                self.worktrees.job.is_none(),
                "Worktree operation is in progress"
            );
            if let Some(reason) = block_reason(info, &self.worktree_used_paths()) {
                anyhow::bail!(reason);
            }
        }
        let (title, body, accept) = match kind {
            "info" => {
                let branch = info.branch.clone().unwrap_or_else(|| {
                    format!("Detached at {}", info.head.chars().take(8).collect::<String>())
                });
                let changes = info.changes.as_ref().map(|c| {
                    format!("Staged: {}\r\nUnstaged: {}\r\nUntracked: {}", c.staged, c.unstaged, c.untracked)
                }).unwrap_or_else(|| "Status unavailable".into());
                let time = info.commit_time.and_then(|t| chrono::DateTime::from_timestamp(t, 0))
                    .map(|t| t.to_rfc3339()).unwrap_or_else(|| "Unavailable".into());
                let mut body = format!(
                    "Branch: {branch}\r\nPath: {}\r\n\r\nCommit: {}\r\n{}\r\nCommit time: {time}\r\n\r\n{changes}",
                    info.path.display(), info.head, info.commit_subject.as_deref().unwrap_or("Description unavailable")
                );
                if let Some(reason) = block_reason(info, &self.worktree_used_paths()) {
                    body.push_str(&format!("\r\n\r\n{reason}"));
                }
                if let Some(reason) = &info.prunable_reason {
                    body.push_str(&format!("\r\nPrunable: {reason}"));
                }
                (branch, body, None)
            }
            "force" => ("Force remove dirty worktree?".into(), format!("This worktree contains changes. Removing it will discard uncommitted and untracked files. The Git branch will remain.\r\n\r\n{}", path.display()), Some("Force remove")),
            _ => ("Remove worktree?".into(), format!("The checkout directory will be removed. Its Git branch will remain.\r\n\r\n{}", path.display()), Some("Remove")),
        };
        let id = Uuid::new_v4();
        let panel = editor::ClosePanel::worktree(
            self.window,
            id,
            &title,
            &body,
            accept,
            self.background_test,
        )?;
        self.worktrees.decision = Some(Decision {
            id,
            kind,
            path,
            panel,
        });
        Ok(())
    }
    fn worktree_remove(&mut self, path: PathBuf, force: bool) -> anyhow::Result<()> {
        self.worktree_guard()?;
        anyhow::ensure!(
            self.worktrees.job.is_none(),
            "Worktree operation is in progress"
        );
        let list = self
            .worktrees
            .list
            .as_ref()
            .context("Worktree list is unavailable")?;
        let row = list
            .items
            .iter()
            .find(|row| row.path == path)
            .context("Worktree is no longer in the list")?;
        let used = self.worktree_used_paths();
        if let Some(reason) = block_reason(row, &used) {
            anyhow::bail!(reason);
        }
        let root = list.repository_root.clone();
        let source = self
            .worktrees
            .source
            .clone()
            .context("Worktree source is unavailable")?;
        let id = Uuid::new_v4();
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let sender = self.sender.clone();
        std::thread::Builder::new()
            .name("worktree-remove".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(|| {
                    // Resolve local aliases before waiting on other native windows.
                    super::super::worktree_git::ensure_unused(&path, &used)
                        .map_err(RemoveError::Failed)?;
                    let other = super::super::ipc::other_window_paths(&token)
                        .map_err(|e| RemoveError::Failed(e.to_string()))?;
                    super::super::worktree_git::ensure_unused(&path, &other)
                        .map_err(RemoveError::Failed)?;
                    super::super::worktree_git::remove(&root, &path, force, &token)
                })
                .unwrap_or_else(|_| Err(RemoveError::Failed("Worktree worker failed".into())));
                sender.send(Event::Worktrees(Signal::Removed(id, path, result)));
            })?;
        self.worktrees.error.clear();
        self.worktrees.job = Some(Job {
            id,
            source,
            cancel,
            removing: true,
        });
        self.worktrees_render()
    }
    pub(super) fn worktrees_event(&mut self, signal: Signal) -> anyhow::Result<()> {
        match signal {
            Signal::Ui(id, action) => {
                if id != self.worktrees.id || !self.worktrees.open {
                    return Ok(());
                }
                if matches!(action, worktree_panel::UiAction::Layout) {
                    return self.layout();
                }
                let result = (|| -> anyhow::Result<()> {
                    self.worktree_guard()?;
                    match action {
                        worktree_panel::UiAction::Select(path) => {
                            if let Some(panel) = &mut self.worktrees.panel {
                                panel.select(&path);
                            }
                        }
                        worktree_panel::UiAction::Navigate(key) => {
                            if let Some(panel) = &mut self.worktrees.panel {
                                panel.navigate(key);
                            }
                        }
                        worktree_panel::UiAction::FocusOut(direction) => {
                            self.worktree_focus_out(direction)?
                        }
                        worktree_panel::UiAction::Close => self.worktrees.shutdown(),
                        worktree_panel::UiAction::Refresh => {
                            self.worktrees.refresh = true;
                            self.worktrees.error.clear();
                        }
                        worktree_panel::UiAction::Info(path) => {
                            self.worktree_dialog("info", path)?
                        }
                        worktree_panel::UiAction::Remove(path) => {
                            self.worktree_dialog("remove", path)?
                        }
                        worktree_panel::UiAction::Layout => {}
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    self.worktrees.error = error.to_string();
                }
                self.worktrees_reconcile()?;
                self.layout()?;
            }
            Signal::Choice(id, accepted) => {
                if self.worktrees.decision.as_ref().is_none_or(|d| d.id != id) {
                    return Ok(());
                }
                let decision = self.worktrees.decision.take().unwrap();
                let kind = decision.kind;
                let path = decision.path.clone();
                drop(decision);
                if accepted && kind != "info" {
                    if let Err(error) = self.worktree_remove(path, kind == "force") {
                        self.worktrees.error = error.to_string();
                    }
                }
                self.worktrees_render()?;
            }
            Signal::Loaded(id, result) => {
                if self.worktrees.job.as_ref().is_none_or(|job| job.id != id) {
                    return Ok(());
                }
                let job = self.worktrees.job.take().unwrap();
                if self.worktrees.open
                    && self.worktrees.source.as_ref() == Some(&job.source)
                    && !job.cancel.load(Ordering::Acquire)
                {
                    match result {
                        Ok(list) => {
                            self.worktrees.list = Some(list);
                            self.worktrees.error.clear();
                        }
                        Err(error) => {
                            self.worktrees.list = None;
                            self.worktrees.error = error;
                        }
                    }
                }
                self.worktrees_reconcile()?;
            }
            Signal::Removed(id, path, result) => {
                if self.worktrees.job.as_ref().is_none_or(|job| job.id != id) {
                    return Ok(());
                }
                let job = self.worktrees.job.take().unwrap();
                if self.worktrees.open
                    && self.worktrees.source.as_ref() == Some(&job.source)
                    && !job.cancel.load(Ordering::Acquire)
                {
                    match result {
                        Ok(()) => self.worktrees.refresh = true,
                        Err(RemoveError::RequiresForce(_)) => {
                            if let Err(error) = self.worktree_dialog("force", path) {
                                self.worktrees.error = format!("{error}. Retry Remove to review the force removal confirmation.");
                            }
                        }
                        Err(RemoveError::Locked(error) | RemoveError::Failed(error)) => {
                            self.worktrees.error = error
                        }
                    }
                }
                self.worktrees_reconcile()?;
            }
        }
        Ok(())
    }
}
