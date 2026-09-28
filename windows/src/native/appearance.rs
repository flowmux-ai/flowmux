// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use crate::{command::SettingsOp, settings::SettingKey};
#[path = "options_panel.rs"]
mod options_panel;
pub(super) use options_panel::{Panel, UiAction};

impl App {
    pub(super) fn settings_status(&self) -> Value {
        json!({"document":self.settings,"path":self.settings_worker.path,
            "persistent":self.settings_worker.path.is_some(),"config_error":self.settings_error,
            "pending_writes":self.settings_pending.len(),
            "options":self.options.as_ref().map(Panel::diagnostics),
            "surfaces":self.surfaces.iter().map(|(id,s)|json!({"surface":id,"applied":s.applied_settings})).collect::<Vec<_>>()})
    }
    pub(super) fn settings_submit(
        &mut self,
        op: SettingsOp,
        reply: Option<ipc::Reply>,
        editor: Option<Uuid>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.close_request.is_none(),
            "window is saving before close"
        );
        let id = Uuid::new_v4();
        self.settings_worker.submit(id, op)?;
        self.settings_pending.insert(id, (reply, editor));
        Ok(())
    }
    pub(super) fn settings_result(
        &mut self,
        update: super::super::settings_store::Update,
    ) -> anyhow::Result<()> {
        let pending = update
            .request
            .and_then(|id| self.settings_pending.remove(&id));
        let options_request = pending.as_ref().and_then(|(_, editor)| *editor);
        let result = match update.result {
            Ok(document) => {
                self.settings_error = None;
                if self.settings != document {
                    self.settings = document;
                    chrome::window_theme(self.window, self.settings.terminal.theme);
                    chrome::configure(self.settings.terminal.theme, unsafe {
                        GetDpiForWindow(self.window)
                    });
                    unsafe {
                        InvalidateRect(self.window, std::ptr::null(), 1);
                    }
                    for surface in self.surfaces.values_mut() {
                        surface.applied_settings = None;
                        if surface.ready || surface.restoring {
                            if let Err(error) = surface.send(&HostMessage::Settings {
                                document: self.settings.clone(),
                                bindings: crate::keybindings::resolved(&self.settings.keybindings)?,
                            }) {
                                report(&format!("settings delivery: {error:#}"));
                            }
                        }
                    }
                }
                Ok(())
            }
            Err(error) => {
                self.settings_error = Some(error.clone());
                Err(error)
            }
        };
        if let Some(panel) = self.options.as_mut() {
            panel.update(
                &self.settings,
                self.settings_error.as_deref(),
                options_request == Some(panel.edit_id),
            );
        }
        if let Some((reply, editor)) = pending {
            if let Some(reply) = reply {
                let value = match &result {
                    Ok(()) => self.settings_status(),
                    Err(error) => json!({"error":error}),
                };
                let _ = reply.try_send(value);
            }
            if let Some(editor) = editor {
                if let Some(panel) = self.metadata.as_ref().filter(|p| p.edit_id == editor) {
                    match &result {
                        Ok(()) => panel.hide(),
                        Err(error) => panel.status(error),
                    }
                }
            }
        }
        for control in &self.controls {
            if matches!(control.action, Action::Settings) {
                unsafe {
                    SetWindowTextW(
                        control.hwnd,
                        wide(if self.settings_error.is_some() {
                            "Options (!)…"
                        } else {
                            "Options…"
                        })
                        .as_ptr(),
                    );
                }
            }
        }
        Ok(())
    }
    pub(super) fn settings_menu(&mut self) -> anyhow::Result<()> {
        if self.options.is_none() {
            self.options = Some(Panel::new(self.window)?);
        }
        self.options.as_mut().unwrap().show(
            &self.settings,
            self.settings_error.as_deref(),
            self.background_test,
        );
        Ok(())
    }
    pub(super) fn options_handle_message(&self, message: &MSG) -> bool {
        self.options
            .as_ref()
            .is_some_and(|panel| panel.handle_message(message))
    }
    pub(super) fn options_ui(&mut self, action: UiAction) -> anyhow::Result<()> {
        let Some(panel) = self.options.as_mut() else {
            return Ok(());
        };
        let index = match action {
            UiAction::Layout => {
                panel.layout();
                return Ok(());
            }
            UiAction::Tab(page) => {
                panel.select(page);
                return Ok(());
            }
            UiAction::Close => {
                panel.hide();
                if !self.background_test {
                    self.focus_active()?;
                }
                self.options_save_next()?;
                return Ok(());
            }
            UiAction::Reload => {
                if panel.reset_ready() {
                    panel.reload(&self.settings, self.settings_error.as_deref());
                }
                return Ok(());
            }
            UiAction::Bindings(signal) => {
                panel.bindings_signal(signal);
                return self.options_save_next();
            }
            UiAction::Changed(index) => {
                panel.changed(index);
                return self.options_save_next();
            }
            UiAction::Save => return self.options_save_next(),
            UiAction::Scroll(delta) => {
                panel.scroll_by(delta);
                return Ok(());
            }
            UiAction::ScrollTo(position) => {
                panel.scroll_to(position);
                return Ok(());
            }
            UiAction::Reveal(index) => {
                panel.reveal(index);
                return Ok(());
            }
            UiAction::Reset => usize::MAX,
        };
        let operation = if index == usize::MAX {
            if !panel.reset_ready() {
                return Ok(());
            }
            Ok(SettingsOp::Reset)
        } else {
            panel.operation(index, &self.settings)
        };
        let edit_id = panel.edit_id;
        let result =
            operation.and_then(|operation| self.settings_submit(operation, None, Some(edit_id)));
        if let Some(panel) = self.options.as_mut() {
            match result {
                Ok(()) => panel.begin(index),
                Err(error) => panel.failed(index, &format!("{error:#}")),
            }
        }
        Ok(())
    }
    fn options_save_next(&mut self) -> anyhow::Result<()> {
        // At most ten coalesced fields, never a queue of individual keystrokes.
        while let Some(index) = self.options.as_mut().and_then(Panel::next_due) {
            let panel = self.options.as_ref().unwrap();
            let edit_id = panel.edit_id;
            let operation = panel.operation(index, &self.settings);
            let result = operation.and_then(|op| self.settings_submit(op, None, Some(edit_id)));
            let panel = self.options.as_mut().unwrap();
            match result {
                Ok(()) => {
                    panel.begin(index);
                    break;
                }
                Err(error) => panel.failed(index, &format!("{error:#}")),
            }
        }
        Ok(())
    }
}
