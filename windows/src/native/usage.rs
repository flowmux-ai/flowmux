// SPDX-License-Identifier: GPL-3.0-or-later
//! Usage refreshes share one cancellable worker and retain successful fields.
use super::*;
use crate::usage::{
    FieldRefresh, Provider, ProviderRefresh, UsageError, UsageErrorKind, UsagePanelState,
};
use chrono::Utc;
use std::sync::atomic::{AtomicBool, Ordering};

struct Job {
    id: Uuid,
    cancel: Arc<AtomicBool>,
    started: Instant,
    synthetic: bool,
    retired: bool,
}
#[derive(Default)]
pub(super) struct Controller {
    state: UsagePanelState,
    panel: Option<usage_panel::Panel>,
    bar: Option<usage_panel::Bar>,
    job: Option<Job>,
    next_refresh: Option<Instant>,
    retry: usize,
}
impl Controller {
    pub(super) fn hide_panel(&self) {
        if let Some(panel) = &self.panel {
            panel.hide();
        }
    }
    pub(super) fn is_open(&self) -> bool {
        self.panel.as_ref().is_some_and(|p| p.is_open())
    }
    pub(super) fn reposition(&self) {
        if let Some(panel) = self.panel.as_ref().filter(|p| p.is_open()) {
            panel.position();
        }
    }
    pub(super) fn handle_message(&self, message: &MSG) -> bool {
        self.panel
            .as_ref()
            .is_some_and(|panel| panel.handle_message(message))
    }
    pub(super) fn status(&self) -> Value {
        json!({"state":self.state,"worker_active":self.job.is_some(),"retry":self.retry,
            "job_id":self.job.as_ref().map(|job|job.id),"retired":self.job.as_ref().is_some_and(|job|job.retired),
            "panel":self.panel.as_ref().map(usage_panel::Panel::diagnostics),
            "bar":self.bar.as_ref().map(usage_panel::Bar::diagnostics)})
    }
    pub(super) fn capture_window(&self) -> Option<HWND> {
        self.panel
            .as_ref()
            .filter(|p| p.is_open())
            .map(|p| p.window)
    }
    pub(super) fn shutdown(&mut self) {
        if let Some(job) = &mut self.job {
            job.retired = true;
            job.cancel.store(true, Ordering::Release);
        }
        // Keep a cancelled real collector until its completion arrives: reopening
        // must not create a second account collector while the first drains.
        if self.job.as_ref().is_some_and(|job| job.synthetic) {
            self.job.take();
        }
        self.panel.take();
        self.bar.take();
        self.state.refreshing = false;
        self.next_refresh = None;
        self.retry = 0;
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        self.shutdown();
    }
}
impl App {
    pub(super) fn usage_initialize(&mut self) -> anyhow::Result<()> {
        if self.main_closed || self.usage.bar.is_some() {
            return Ok(());
        }
        self.usage.bar = Some(usage_panel::Bar::new(self.window)?);
        self.usage_render();
        // A recently completed refresh may reject collection below. Reopening
        // still needs the periodic refresh that shutdown cleared.
        self.usage.next_refresh = Some(Instant::now() + Duration::from_secs(150));
        // Hidden verification must never read a developer's login or contact an account.
        if !self.background_test {
            self.usage_refresh(false)?;
        }
        Ok(())
    }
    pub(super) fn usage_render(&self) {
        let enabled = self.settings.terminal.usage_bar_enabled;
        if let Some(panel) = &self.usage.panel {
            panel.update(&self.usage.state, enabled);
        }
        if let Some(bar) = &self.usage.bar {
            bar.update(&self.usage.state, enabled);
        }
    }
    pub(super) fn usage_height(&self, client_height: i32) -> i32 {
        let dpi = unsafe { GetDpiForWindow(self.window) }.max(96);
        self.usage
            .bar
            .as_ref()
            .map_or(0, |bar| bar.height(dpi))
            .min((client_height - (80 * dpi as i32 / 96)).max(0))
    }
    pub(super) fn usage_layout(&self, client: RECT, sidebar: i32) {
        let height = self.usage_height(client.bottom);
        if let Some(bar) = &self.usage.bar {
            bar.layout(
                RECT {
                    left: sidebar,
                    top: client.bottom - height,
                    right: client.right,
                    bottom: client.bottom,
                },
                self.background_test,
            );
        }
        if let Some(panel) = &self.usage.panel {
            panel.layout();
        }
    }
    pub(super) fn toggle_usage(&mut self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.main_closed
                && !self.closing
                && !self.close_accepted
                && self.close_request.is_none()
                && self.pending_save.is_none()
                && self.editor_barrier.is_none()
                && !self.overview.is_open()
                && !self.command_palette.is_open()
                && unsafe { IsWindowEnabled(self.window) } != 0,
            "Window is busy"
        );
        if self.usage.panel.as_ref().is_some_and(|p| p.is_open()) {
            self.usage.panel.as_ref().unwrap().hide();
            return Ok(());
        }
        if self.usage.panel.is_none() {
            self.usage.panel = Some(usage_panel::Panel::new(self.window)?);
        }
        self.usage_refresh(false)?;
        self.usage_render();
        let anchor = self
            .controls
            .iter()
            .find(|c| matches!(c.action, Action::Usage))
            .map_or(self.window, |c| c.hwnd);
        self.usage
            .panel
            .as_ref()
            .unwrap()
            .show(self.background_test, anchor);
        Ok(())
    }
    pub(super) fn usage_refresh(&mut self, force: bool) -> anyhow::Result<()> {
        if self.usage.job.is_some() || self.main_closed || self.closing || self.close_accepted {
            return Ok(());
        }
        let admitted = if force {
            self.usage.state.begin_forced_refresh()
        } else {
            self.usage.state.begin_refresh(Utc::now())
        };
        if !admitted {
            return Ok(());
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let id = Uuid::new_v4();
        self.usage.job = Some(Job {
            id,
            cancel: cancel.clone(),
            started: Instant::now(),
            synthetic: self.background_test,
            retired: false,
        });
        self.usage.next_refresh = None;
        self.usage_render();
        // TestUsage supplies responses only in hidden debug hosts; production always collects.
        if self.background_test {
            return Ok(());
        }
        let sender = self.sender.clone();
        if std::thread::Builder::new()
            .name("flowmux-usage".into())
            .spawn(move || {
                let result =
                    std::panic::catch_unwind(|| super::super::usage_collect::collect(&cancel))
                        .unwrap_or_else(|_| {
                            failed(UsageErrorKind::Io, "The usage collector failed.")
                        });
                sender.send(Event::UsageResult(id, result));
            })
            .is_err()
        {
            self.usage_complete(
                id,
                failed(UsageErrorKind::Io, "Could not start the usage collector."),
            )?;
        }
        Ok(())
    }
    pub(super) fn usage_complete(
        &mut self,
        id: Uuid,
        results: [ProviderRefresh; 2],
    ) -> anyhow::Result<()> {
        if self.usage.job.as_ref().is_none_or(|job| job.id != id) {
            return Ok(());
        }
        let job = self.usage.job.take().unwrap();
        if self.main_closed || job.retired {
            if !self.main_closed {
                self.usage_refresh(false)?;
            }
            return Ok(());
        }
        self.usage_result(results)
    }
    pub(super) fn usage_result(&mut self, results: [ProviderRefresh; 2]) -> anyhow::Result<()> {
        if self.main_closed {
            return Ok(());
        }
        self.usage.job.take();
        for result in results {
            self.usage.state.apply(result);
        }
        self.usage.state.finish_refresh(Utc::now());
        let failed = self.usage.state.claude.limits_error.is_some()
            || self.usage.state.codex.limits_error.is_some();
        let delay = if failed && self.usage.retry < 3 {
            let delay = [5, 20, 60][self.usage.retry];
            self.usage.retry += 1;
            delay
        } else {
            self.usage.retry = if failed { 4 } else { 0 };
            150
        };
        self.usage.next_refresh = Some(Instant::now() + Duration::from_secs(delay));
        self.usage_render();
        self.layout()
    }
    pub(super) fn usage_tick(&mut self) -> anyhow::Result<()> {
        if self.main_closed {
            return Ok(());
        }
        if let Some(job) = &self.usage.job {
            if job.retired {
                return Ok(());
            }
            if job.started.elapsed() >= Duration::from_secs(20) {
                job.cancel.store(true, Ordering::Release);
                if self.background_test {
                    self.usage_result(failed(
                        UsageErrorKind::Timeout,
                        "The usage request timed out.",
                    ))?;
                }
            }
            return Ok(());
        }
        if !self.background_test
            && self
                .usage
                .next_refresh
                .is_some_and(|due| Instant::now() >= due)
            && (self.settings.terminal.usage_bar_enabled || (1..=3).contains(&self.usage.retry))
        {
            self.usage_refresh(true)?;
        }
        Ok(())
    }
    pub(super) fn usage_ui(&mut self, action: usage_panel::UiAction) -> anyhow::Result<()> {
        if self.main_closed {
            return Ok(());
        }
        use usage_panel::UiAction;
        match action {
            UiAction::Refresh => self.usage_refresh(true)?,
            UiAction::ToggleBar(enabled) => {
                self.settings_submit(
                    crate::command::SettingsOp::Set {
                        key: crate::settings::SettingKey::UsageBarEnabled,
                        value: enabled.to_string(),
                        expected: Some(self.settings.terminal.usage_bar_enabled.to_string()),
                    },
                    None,
                    None,
                )?;
            }
            action => {
                if let Some(panel) = &self.usage.panel {
                    match action {
                        UiAction::Close => panel.hide(),
                        UiAction::Layout => panel.layout(),
                        UiAction::Scroll(delta) => panel.scroll_by(delta),
                        UiAction::ScrollTo(offset) => panel.scroll(offset),
                        _ => {}
                    }
                }
            }
        }
        Ok(())
    }
    #[cfg(debug_assertions)]
    pub(super) fn test_usage(&mut self, input: &str) -> anyhow::Result<Value> {
        anyhow::ensure!(
            self.background_test,
            "Usage injection requires an owned hidden debug host"
        );
        anyhow::ensure!(input.len() <= 65536, "Usage test input exceeds 64 KiB");
        let value: Value = serde_json::from_str(input)?;
        let refresh = |provider, key: &str| {
            let value = &value[key];
            let error = || UsageError::new(UsageErrorKind::Network, "Usage service unavailable.");
            let tokens = if value["token_error"].as_bool() == Some(true) {
                Err(error())
            } else {
                match provider {
                    Provider::Claude => Ok(crate::usage::TokenTotals {
                        today: value["today"].as_u64(),
                        lifetime: None,
                    }),
                    Provider::Codex => crate::usage::codex_tokens(
                        &value["tokens"],
                        chrono::Local::now().date_naive(),
                    ),
                }
            };
            let limits = if value["limits_error"].as_bool() == Some(true) {
                Err(error())
            } else {
                match provider {
                    Provider::Claude => crate::usage::claude_limits(&value["limits"]),
                    Provider::Codex => crate::usage::codex_limits(&value["limits"]),
                }
            };
            ProviderRefresh {
                provider,
                tokens: tokens.map_or_else(FieldRefresh::Failure, FieldRefresh::Success),
                limits: limits.map_or_else(FieldRefresh::Failure, FieldRefresh::Success),
                collected_at: Utc::now(),
            }
        };
        let results = [
            refresh(Provider::Claude, "claude"),
            refresh(Provider::Codex, "codex"),
        ];
        if let Some(id) = value.get("job_id") {
            self.usage_complete(serde_json::from_value(id.clone())?, results)?;
        } else if let Some(job) = &self.usage.job {
            self.usage_complete(job.id, results)?;
        } else {
            self.usage_result(results)?;
        }
        Ok(self.usage.status())
    }
}
fn failed(kind: UsageErrorKind, message: &str) -> [ProviderRefresh; 2] {
    [Provider::Claude, Provider::Codex].map(|provider| ProviderRefresh {
        provider,
        tokens: FieldRefresh::Failure(UsageError::new(kind, message)),
        limits: FieldRefresh::Failure(UsageError::new(kind, message)),
        collected_at: Utc::now(),
    })
}
